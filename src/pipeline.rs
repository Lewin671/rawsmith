//! The fixed developing pipeline, in two halves:
//!
//! 1. [`scene`]: sensor data to a scene-linear working-space image (white
//!    balance, demosaic, highlight reconstruction, lens corrections, colour
//!    matrix, noise reduction, geometry, resize).
//! 2. [`finish`]: scene-linear to an sRGB-encoded picture (exposure, dehaze,
//!    local tone, filmic curve, white/black points, curve, colour, sharpening,
//!    gamut mapping).
//!
//! The split lets automatic adjustment evaluate many tone settings on one
//! prepared scene image.

use rayon::prelude::*;
use serde::Serialize;

use crate::adjust::ColorOps;
use crate::arw::RawImage;
use crate::color::{self, srgb_encode, Spaces};
use crate::error::{Error, Result};
use crate::image::{guided_filter, Image, Plane};
use crate::params::Params;
use crate::tone::{self, Filmic, Local};
use crate::{demosaic, lens};

/// Gain, in stops, between sensor-linear data and scene-linear values where
/// 0.18 is middle grey: the camera's metering leaves this much room for
/// highlights.
pub const BASE_EXPOSURE: f32 = 1.2;

#[derive(Debug, Clone, Serialize)]
pub struct WhiteBalance {
    pub temperature: f64,
    pub tint: f64,
    pub as_shot_temperature: f64,
    pub as_shot_tint: f64,
}

pub struct Scene {
    /// Scene-linear working-space image, upright and cropped.
    pub image: Image,
    pub white_balance: WhiteBalance,
    /// Percent of pixels where at least one channel saturated on the sensor.
    pub sensor_clipped_pct: f64,
    /// Pixels per pixel of the full-resolution upright image.
    pub scale: f32,
}

fn camera_matrix(raw: &RawImage) -> Result<color::Mat3> {
    let model = raw.meta.model.as_deref().unwrap_or("unknown camera");
    color::camera_matrix(model).ok_or_else(|| {
        Error::Unsupported(format!(
            "no colour profile for {model}; supported cameras: {}",
            color::supported_cameras().join(", ")
        ))
    })
}

/// The white balance a recipe asks for, and the multipliers that apply it.
pub fn white_balance(raw: &RawImage, params: &Params) -> Result<(WhiteBalance, [f32; 3])> {
    let cam = camera_matrix(raw)?;
    let (as_shot_t, as_shot_tint) = color::temperature_from_wb(&cam, raw.wb);
    let temperature = params.temperature.unwrap_or(as_shot_t);
    let tint = params.tint.unwrap_or(as_shot_tint);
    // With nothing overridden use the camera's multipliers exactly.
    let gains = if params.temperature.is_none() && params.tint.is_none() {
        raw.wb
    } else {
        color::wb_from_temperature(&cam, temperature, tint)
    };
    let info = WhiteBalance {
        temperature: temperature.round(),
        tint: (tint * 10.0).round() / 10.0,
        as_shot_temperature: as_shot_t.round(),
        as_shot_tint: (as_shot_tint * 10.0).round() / 10.0,
    };
    Ok((info, gains))
}

/// Rotates the image content clockwise by `degrees` about its centre and
/// crops to the largest rectangle of the same aspect ratio with no empty corners.
fn rotated(image: &Image, degrees: f32) -> Image {
    let theta = degrees.to_radians();
    let (sin, cos) = theta.sin_cos();
    let (w, h) = (image.width as f32, image.height as f32);
    let (long, short) = (w.max(h), w.min(h));
    let keep = short / (long * sin.abs() + short * cos.abs());
    let (ow, oh) = (((w * keep) as usize).max(1), ((h * keep) as usize).max(1));
    let mut out = Image::new(ow, oh);
    let (cx, cy) = ((w - 1.0) * 0.5, (h - 1.0) * 0.5);
    let (ocx, ocy) = ((ow as f32 - 1.0) * 0.5, (oh as f32 - 1.0) * 0.5);
    out.data.par_chunks_mut(ow).enumerate().for_each(|(y, row)| {
        let dy = y as f32 - ocy;
        for (x, p) in row.iter_mut().enumerate() {
            let dx = x as f32 - ocx;
            // Inverse mapping: where in the source this output pixel came from.
            *p = image.sample(cx + dx * cos + dy * sin, cy - dx * sin + dy * cos);
        }
    });
    out
}

/// Smooths chroma (and, at high strength, luminance) noise. Works on
/// square-root encoded values, where sensor noise is roughly uniform.
fn denoise(image: &mut Image, strength: f32) {
    if strength <= 0.0 {
        return;
    }
    let encoded: Vec<[f32; 3]> = image.data.par_iter().map(|p| p.map(|v| v.max(0.0).sqrt())).collect();
    let (w, h) = (image.width, image.height);
    let plane = |f: &(dyn Fn(&[f32; 3]) -> f32 + Sync)| Plane {
        width: w,
        height: h,
        data: encoded.par_iter().map(f).collect(),
    };
    let y = plane(&|p| (p[0] + 2.0 * p[1] + p[2]) * 0.25);
    let cr = plane(&|p| p[0] - p[1]);
    let cb = plane(&|p| p[2] - p[1]);
    let radius = (2.0 + strength * 6.0) as usize;
    let eps = 0.0004 * strength * strength + 1e-6;
    // Chroma follows luminance edges, so colour does not bleed across them.
    let cr = guided_filter(&y, &cr, radius, eps);
    let cb = guided_filter(&y, &cb, radius, eps);
    // Luminance smoothing only in the upper half of the slider.
    let luma_strength = ((strength - 0.5) * 2.0).max(0.0);
    let y = if luma_strength > 0.0 { guided_filter(&y, &y, 2, 0.0003 * luma_strength * luma_strength) } else { y };
    image.data.par_iter_mut().enumerate().for_each(|(i, p)| {
        // Invert: y = (r + 2g + b) / 4 with r = g + cr, b = g + cb.
        let g = y.data[i] - (cr.data[i] + cb.data[i]) * 0.25;
        let (r, b) = (g + cr.data[i], g + cb.data[i]);
        *p = [r.max(0.0).powi(2), g.max(0.0).powi(2), b.max(0.0).powi(2)];
    });
}

/// How large to render.
#[derive(Debug, Clone, Copy, Default)]
pub struct Size {
    /// Longest edge of the output in pixels; `None` renders full resolution.
    pub max_edge: Option<usize>,
}

/// Builds the scene-linear image for a recipe.
pub fn scene(raw: &RawImage, params: &Params, size: Size) -> Result<Scene> {
    let cam_to_work = color::camera_to_working(&camera_matrix(raw)?);
    let (white_balance, gains) = white_balance(raw, params)?;
    let mosaic = demosaic::prepare(raw, gains);

    // What the final picture's long edge will be at full resolution.
    let crop = &params.crop;
    let (cw, ch) = ((crop.right - crop.left) as f32, (crop.bottom - crop.top) as f32);
    let upright = if matches!(raw.meta.orientation, 6 | 8) {
        (mosaic.height, mosaic.width)
    } else {
        (mosaic.width, mosaic.height)
    };
    let full_edge = (upright.0 as f32 * cw).max(upright.1 as f32 * ch);
    // Half-resolution demosaicing is exact (no interpolation) and four times
    // cheaper, so use it whenever the output is small enough.
    let half = size.max_edge.is_some_and(|edge| edge as f32 <= full_edge * 0.5);

    let mut image = if half { demosaic::half(&mosaic) } else { demosaic::full(&mosaic) };
    let clipped = demosaic::reconstruct_highlights(&mut image, mosaic.clip);
    drop(mosaic);
    if params.lens_corrections {
        if let Some(tables) = &raw.lens {
            image = lens::correct(&image, tables);
        }
    }
    image.map(|p| color::apply(&cam_to_work, p));
    if !half {
        denoise(&mut image, params.denoise as f32 / 100.0);
    }

    let mut image = image.oriented(raw.meta.orientation);
    if params.rotate != 0.0 {
        image = rotated(&image, params.rotate as f32);
    }
    if *crop != Default::default() {
        let (w, h) = (image.width as f64, image.height as f64);
        let (x0, y0) = ((crop.left * w).round() as usize, (crop.top * h).round() as usize);
        let (x1, y1) = ((crop.right * w).round() as usize, (crop.bottom * h).round() as usize);
        image = image.crop(x0, y0, (x1 - x0).max(1), (y1 - y0).max(1));
    }
    let mut scale = if half { 0.5 } else { 1.0 };
    if let Some(edge) = size.max_edge {
        let long = image.width.max(image.height);
        if long > edge {
            let k = edge as f32 / long as f32;
            image = image.resized(
                ((image.width as f32 * k).round() as usize).max(1),
                ((image.height as f32 * k).round() as usize).max(1),
            );
            scale *= k;
        }
    }
    Ok(Scene { image, white_balance, sensor_clipped_pct: (clipped as f64 * 10_000.0).round() / 100.0, scale })
}

/// Unsharp mask on the encoded image's luminance, limited to the local
/// range so edges do not ring.
fn sharpen(image: &mut Image, amount: f32) {
    if amount <= 0.0 {
        return;
    }
    let y = Plane::from_fn(image, |p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]);
    let blurred = y.blur(0.9);
    let (w, h) = (image.width, image.height);
    image.data.par_chunks_mut(w).enumerate().for_each(|(row, pixels)| {
        for (x, p) in pixels.iter_mut().enumerate() {
            let i = row * w + x;
            let detail = y.data[i] - blurred.data[i];
            // Leave the faintest differences alone: they are mostly noise.
            let detail = detail - detail.clamp(-0.004, 0.004);
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for ny in row.saturating_sub(1)..=(row + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    lo = lo.min(y.data[ny * w + nx]);
                    hi = hi.max(y.data[ny * w + nx]);
                }
            }
            // Never brighter or darker than the neighbours: edges steepen but cannot halo.
            let slack = 0.03 * (hi - lo);
            let target = (y.data[i] + amount * detail).clamp(lo - slack, hi + slack);
            let delta = target - y.data[i];
            *p = [p[0] + delta, p[1] + delta, p[2] + delta];
        }
    });
}

/// Turns a scene-linear image into the final sRGB-encoded picture.
pub fn finish(scene: &Image, params: &Params, spaces: &Spaces) -> Image {
    let mut image = scene.clone();
    let base = crate::tune("base", BASE_EXPOSURE);
    let gain = (params.exposure as f32 + base).exp2();
    image.map(|p| [p[0] * gain, p[1] * gain, p[2] * gain]);

    if !params.gradients.is_empty() {
        let (w, h) = ((image.width.max(2) - 1) as f32, (image.height.max(2) - 1) as f32);
        let gradients: Vec<_> = params
            .gradients
            .iter()
            .map(|g| {
                let from = [g.from[0] as f32 * w, g.from[1] as f32 * h];
                let axis = [g.to[0] as f32 * w - from[0], g.to[1] as f32 * h - from[1]];
                let scale = 1.0 / (axis[0] * axis[0] + axis[1] * axis[1]).max(1e-6);
                (from, [axis[0] * scale, axis[1] * scale], g.exposure as f32, g.saturation as f32 / 100.0)
            })
            .collect();
        image.map_xy(|x, y, mut p| {
            for (from, axis, exposure, saturation) in &gradients {
                // Position along the gradient: 0 at `from`, 1 at `to`.
                let t = ((x as f32 - from[0]) * axis[0] + (y as f32 - from[1]) * axis[1]).clamp(0.0, 1.0);
                let weight = 1.0 - t * t * (3.0 - 2.0 * t);
                let gain = (exposure * weight).exp2();
                let grey = color::luma(p);
                let chroma = (1.0 + saturation * weight).max(0.0);
                p = p.map(|v| (grey + (v - grey) * chroma) * gain);
            }
            p
        });
    }
    if params.vignette != 0.0 {
        // Up to one stop at the corners, growing with the square of the radius.
        let (cx, cy) = ((image.width as f32 - 1.0) * 0.5, (image.height as f32 - 1.0) * 0.5);
        let (stops, norm) = (params.vignette as f32 / 100.0, 1.0 / (cx * cx + cy * cy).max(1.0));
        image.map_xy(|x, y, p| {
            let r2 = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)) * norm;
            p.map(|v| v * (stops * r2).exp2())
        });
    }
    tone::dehaze(&mut image, params.dehaze as f32 / 100.0);
    tone::local_tone(
        &mut image,
        &Local {
            highlights: params.highlights as f32 / 100.0,
            shadows: params.shadows as f32 / 100.0,
            clarity: params.clarity as f32 / 100.0,
        },
    );

    // Sensor saturation sits at 2^BASE_EXPOSURE and just reaches display white.
    let filmic = Filmic::new(params.contrast as f32, (base + crate::tune("white", 0.0)).exp2());
    // Black and white points on encoded display values: `whites` moves the
    // value that maps to pure white, `blacks` the one that maps to black.
    let black = -params.blacks as f32 / 100.0 * 0.08;
    let white_point = 1.0 - params.whites as f32 / 100.0 * 0.25;
    let curve = (!params.curve.is_empty()).then(|| tone::curve_lut(&params.curve, 1025));
    let scene_white = base.exp2();
    let ops = ColorOps::new(params, spaces);
    // Shadows and midtones keep their hue and saturation under the curve;
    // highlights take the curve per channel, which rolls bright colours off
    // towards yellow and white the way film and cameras do.
    let ratio_strength = crate::tune("mix", 0.5);
    image.map(|p| {
        let per_channel = p.map(|v| filmic.eval(v));
        let y = color::luma(p).max(1e-6);
        let k = filmic.eval(y) / y;
        let t = ((y - tone::GREY) / (0.6 * scene_white - tone::GREY)).clamp(0.0, 1.0);
        let ratio_mix = ratio_strength * (1.0 - t * t * (3.0 - 2.0 * t));
        let mut p = [0, 1, 2].map(|c| per_channel[c] + (p[c].max(0.0) * k - per_channel[c]) * ratio_mix);
        if black != 0.0 || white_point != 1.0 || curve.is_some() {
            p = p.map(|v| {
                let mut e = srgb_encode(v);
                e = ((e - black) / (white_point - black)).clamp(0.0, 1.0);
                if let Some(lut) = &curve {
                    e = tone::lut_eval(lut, e);
                }
                color::srgb_decode(e)
            });
        }
        p
    });
    ops.run(&mut image, spaces);
    image.map(|p| spaces.to_srgb_gamut(p).map(srgb_encode));
    sharpen(&mut image, params.sharpen as f32 / 100.0 * 1.5);
    image.map(|p| p.map(|v| v.clamp(0.0, 1.0)));
    image
}

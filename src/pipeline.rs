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
/// highlights. Fitted so the default rendering is as bright as the camera's
/// own JPEG.
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
        color::wb_from_temperature(&cam, temperature, tint).ok_or_else(|| {
            Error::Params(format!(
                "temperature {temperature:.0} K with tint {tint:.0} is outside what this camera can render; move tint closer to 0"
            ))
        })?
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

/// Shared rotation and crop geometry, also applied to the camera comparison.
pub(crate) fn framed(image: Image, orientation: u16, params: &Params) -> Image {
    let crop = &params.crop;
    let mut image = image.oriented(orientation);
    if params.rotate != 0.0 {
        image = rotated(&image, params.rotate as f32);
    }
    if *crop != Default::default() {
        // At least one pixel, and never past the edge, however small the image.
        let span = |from: f64, to: f64, size: usize| {
            let start = ((from * size as f64).round() as usize).min(size - 1);
            let end = ((to * size as f64).round() as usize).clamp(start + 1, size);
            (start, end - start)
        };
        let ((x0, cw), (y0, ch)) =
            (span(crop.left, crop.right, image.width), span(crop.top, crop.bottom, image.height));
        image = image.crop(x0, y0, cw, ch);
    }
    image
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
    drop(encoded);
    let radius = (2.0 + strength * 6.0) as usize;
    let eps = 0.0004 * strength * strength + 1e-6;
    // Chroma follows luminance edges, so colour does not bleed across them.
    let original_cr = cr;
    let original_cb = cb;
    let mut cr = guided_filter(&y, &original_cr, radius, eps);
    let mut cb = guided_filter(&y, &original_cb, radius, eps);
    let coarse_strength = ((strength - 0.25) / 0.35).clamp(0.0, 1.0);
    if coarse_strength > 0.0 {
        // A native-pixel filter removes grain but leaves correlated chroma
        // clouds. Estimate that base at quarter size (48-pixel native radius).
        // A chroma boundary mask keeps strong colour discontinuities,
        // including ones with little luminance contrast. Apply only the base
        // correction so fine, luminance-guided colour detail is retained.
        let (sw, sh) = (w.div_ceil(4), h.div_ceil(4));
        let guides = [original_cr.resized(sw, sh), original_cb.resized(sw, sh), y.resized(sw, sh)];
        let coarse = |chroma: &mut Plane| {
            let small = chroma.resized(sw, sh);
            let base = chroma_base(&small, &guides, 12, 0.0064 * strength * strength);
            let (fx, fy) = (sw as f32 / w as f32, sh as f32 / h as f32);
            chroma.data.par_chunks_mut(w).enumerate().for_each(|(row, values)| {
                let sy = (row as f32 + 0.5) * fy - 0.5;
                for (x, v) in values.iter_mut().enumerate() {
                    let sx = (x as f32 + 0.5) * fx - 0.5;
                    *v += (base.sample(sx, sy) - small.sample(sx, sy)) * coarse_strength;
                }
            });
        };
        coarse(&mut cr);
        coarse(&mut cb);
    }
    drop(original_cr);
    drop(original_cb);
    // Luminance smoothing only in the upper half of the slider.
    let luma_strength = ((strength - 0.5) * 2.0).max(0.0);
    let original_luma = Plane::from_fn(image, |p| color::luma(p).max(0.0).sqrt());
    image.data.par_iter_mut().enumerate().for_each(|(i, p)| {
        // Invert the encoded guide, keeping it unchanged by chroma smoothing.
        let g = y.data[i] - (cr.data[i] + cb.data[i]) * 0.25;
        *p = [g + cr.data[i], g, g + cb.data[i]].map(|v| v.max(0.0).powi(2));
    });
    let guide = Plane::from_fn(image, |p| color::luma(p).max(0.0).sqrt());
    // The chroma-cleaned reconstruction is a lower-noise guide. Preserve
    // true linear luminance as the input rather than using sqrt-RGB's proxy.
    let luma = if luma_strength > 0.0 {
        // At high strength, use the chroma-cleaned luminance estimate as well:
        // the camera colour matrix correlates colour noise with noisy Y.
        // Restoring all of that Y would bring the removed noise back as grain.
        let mut input = original_luma;
        input.data.par_iter_mut().zip(&guide.data).for_each(|(v, g)| *v += (*g - *v) * luma_strength);
        guided_filter(&guide, &input, 2, 0.00005 * luma_strength * luma_strength)
    } else {
        original_luma
    };
    image.data.par_iter_mut().enumerate().for_each(|(i, p)| {
        let gain = luma.data[i].max(0.0).powi(2) / color::luma(*p).max(1e-10);
        *p = p.map(|v| v * gain);
    });
}

/// Remove smooth chroma clouds while protecting discontinuous colour edges.
/// Detect boundaries on prefiltered quarter-size guides to reduce false
/// protection from grain without hiding edges behind prior chroma smoothing.
/// The protection covers the complete two-window support of the base filter.
fn chroma_base(input: &Plane, guides: &[Plane; 3], radius: usize, noise_variance: f32) -> Plane {
    // Follow luminance structure at the broad scale too; self-guidance would
    // suppress true colour carried by feathers, foliage and water ripples.
    let mut base = guided_filter(&guides[2], input, radius, noise_variance / 16.0);
    let (w, h) = (input.width, input.height);
    let guides = guides.each_ref().map(|guide| guide.box_blur(1));
    let mut edges = Plane::new(w, h);
    // Use a conservative two-sample budget; prefiltered samples are correlated.
    let threshold = (2.0 * noise_variance).sqrt();
    for y in 0..h {
        for x in 0..w {
            // Use one mask for both axes: protecting only the stronger axis
            // would rotate the hue of a real colour boundary.
            let gradient = guides
                .iter()
                .map(|guide| {
                    let dx = (guide.data[y * w + (x + 1).min(w - 1)] - guide.data[y * w + x.saturating_sub(1)]).abs();
                    let dy = (guide.data[(y + 1).min(h - 1) * w + x] - guide.data[y.saturating_sub(1) * w + x]).abs();
                    dx.max(dy)
                })
                .fold(0.0, f32::max);
            // A soft transition avoids switching protection at one noise value.
            let t = (gradient / threshold - 1.0).clamp(0.0, 1.0);
            edges.data[y * w + x] = t * t * (3.0 - 2.0 * t);
        }
    }
    let support = radius * 2;
    let protection = edges.box_blur(support);
    // A line occupies one pixel per window side; multiply by its side length
    // to preserve it across the whole support, with a continuous mask.
    let gain = (support * 2 + 1) as f32;
    base.data.par_iter_mut().enumerate().for_each(|(i, v)| {
        let keep = (protection.data[i] * gain).clamp(0.0, 1.0);
        *v += (input.data[i] - *v) * keep;
    });
    base
}

/// How large to render.
#[derive(Debug, Clone, Copy, Default)]
pub struct Size {
    /// Longest edge of the output in pixels; `None` renders full resolution.
    pub max_edge: Option<usize>,
}

impl Size {
    /// Area-average a finished, encoded image; never enlarge it.
    pub fn apply(self, image: Image) -> Image {
        let long = image.width.max(image.height);
        if let Some(edge) = self.max_edge.filter(|&edge| edge < long) {
            let k = edge as f64 / long as f64;
            image.resized(
                ((image.width as f64 * k).round() as usize).max(1),
                ((image.height as f64 * k).round() as usize).max(1),
            )
        } else {
            image
        }
    }
}

/// A finished render and its sensor metadata.
pub struct Render {
    pub image: Image,
    pub white_balance: WhiteBalance,
    pub sensor_clipped_pct: f64,
}

/// Develop at native resolution through sharpening, then resize encoded
/// pixels. Nonlinear tone/gamut operations cannot be commuted with averaging;
/// scaling radii alone would still misrepresent noise, clipping and contrast.
pub fn render(raw: &RawImage, params: &Params, size: Size) -> Result<Render> {
    let Scene { image, white_balance, sensor_clipped_pct, .. } = scene(raw, params, Size::default())?;
    let finished = finish(&image, params, &Spaces::new());
    drop(image);
    Ok(Render { image: size.apply(finished), white_balance, sensor_clipped_pct })
}

/// Builds the scene-linear image for a recipe. A reduced size is an
/// approximation for automatic candidate analysis only; use `render` for
/// previews and exports so every spatial/nonlinear stage precedes resizing.
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
    let clipped = mosaic.saturated_fraction();
    demosaic::reconstruct_highlights(&mut image, mosaic.clip);
    drop(mosaic);
    if params.lens_corrections {
        if let Some(tables) = &raw.lens {
            if let Some(corrected) = lens::correct(&image, tables) {
                image = corrected;
            }
        }
    }
    image.map(|p| color::apply(&cam_to_work, p));
    if !half {
        denoise(&mut image, params.denoise as f32 / 100.0);
    }

    let mut image = framed(image, raw.meta.orientation, params);
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
    let base = BASE_EXPOSURE;
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
    let filmic = Filmic::new(params.contrast as f32, base.exp2());
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
    let ratio_strength = 0.5;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarse_denoise_preserves_weak_colour_texture_carried_by_luminance() {
        let (w, h) = (128, 64);
        // Quarter-size colour texture with no discontinuity large enough to
        // trigger the boundary mask. Colour follows genuine light/shade, as
        // on subtly tinted feathers, foliage or reflected water ripples.
        let texture: Vec<f32> = (0..w * h)
            .map(|i| {
                (std::f32::consts::TAU * (i % w) as f32 / 32.0).sin()
                    * (std::f32::consts::TAU * (i / w) as f32 / 48.0).cos()
            })
            .collect();
        let plane = |offset, amplitude| Plane {
            width: w,
            height: h,
            data: texture.iter().map(|v| offset + amplitude * v).collect(),
        };
        let guides = [plane(0.02, 0.012), plane(-0.01, -0.008), plane(0.25, 0.10)];
        let projection = |output: &Plane, input: &Plane| {
            let mut cross = 0.0f64;
            let mut power = 0.0f64;
            let mean = input.data.iter().map(|v| *v as f64).sum::<f64>() / input.data.len() as f64;
            for y in 16..h - 16 {
                for x in 16..w - 16 {
                    let i = y * w + x;
                    let signal = input.data[i] as f64 - mean;
                    cross += (output.data[i] as f64 - mean) * signal;
                    power += signal * signal;
                }
            }
            cross / power
        };
        for input in &guides[..2] {
            let filtered = chroma_base(input, &guides, 12, 0.0064);
            let self_guided = guided_filter(input, input, 12, 0.0064);
            let kept = projection(&filtered, input);
            let erased = projection(&self_guided, input);
            eprintln!("weak colour texture: luminance-guided {kept:.4}, self-guided control {erased:.4}");
            assert!(kept > 0.80, "true colour texture retained: {kept}");
            assert!(erased < 0.20, "the scene must expose self-guided desaturation: {erased}");
        }
    }

    #[test]
    fn chroma_smoothing_keeps_luminance_texture_and_colour_boundaries() {
        let mut image = Image::new(192, 128);
        image.map_xy(|x, y, _| {
            let texture = 0.002 * (x as f32 * 1.1).sin() * (y as f32 * 0.7).cos();
            // Equal Rec.2020 luminance across a real, strong colour boundary.
            let p = if x < 96 { [0.12, 0.06, 0.02] } else { [0.02, 0.09, 0.12] };
            let gain = (0.07 + texture) / color::luma(p);
            p.map(|v| v * gain)
        });
        let original = image.clone();
        denoise(&mut image, 0.5);
        for (a, b) in image.data.iter().zip(&original.data) {
            assert!((color::luma(*a) - color::luma(*b)).abs() < 1e-7);
        }
        let encoded = |channel: usize| Plane::from_fn(&original, |p| p[channel].sqrt() - p[1].sqrt());
        let y = Plane::from_fn(&original, |p| (p[0].sqrt() + 2.0 * p[1].sqrt() + p[2].sqrt()) * 0.25);
        let guides = [encoded(0).resized(48, 32), encoded(2).resized(48, 32), y.resized(48, 32)];
        for channel in [0, 2] {
            let chroma = encoded(channel);
            let small = guided_filter(&y, &chroma, 8, 0.000401).resized(48, 32);
            let base = chroma_base(&small, &guides, 12, 0.0064);
            for x in [20, 23, 24, 28] {
                let i = 16 * 48 + x;
                assert!(
                    (base.data[i] - small.data[i]).abs() < 0.01,
                    "channel {channel}, x {x}: {}",
                    base.data[i] - small.data[i]
                );
            }
        }
        for strength in [0.85, 1.0] {
            let mut image = original.clone();
            denoise(&mut image, strength);
            for x in [64, 128] {
                let (p, q) = (image.at(x, 64), original.at(x, 64));
                // Preserve hue/chroma on both sides as well as luminance.
                for channel in [0, 2] {
                    assert!(((p[channel] - p[1]) - (q[channel] - q[1])).abs() < 0.01);
                }
            }
        }
    }
}

//! The fixed developing pipeline, in two halves:
//!
//! 1. [`scene`]: sensor data to a scene-linear working-space image (white
//!    balance, demosaic, highlight reconstruction, lens corrections, colour
//!    matrix, defringe, noise reduction, geometry). Reduced automatic analysis
//!    scenes may resize here; output rendering stays at native resolution.
//! 2. [`finish`]: scene-linear to an sRGB-encoded picture (exposure, dehaze,
//!    local tone, filmic curve, white/black points, curve, colour, sharpening,
//!    gamut mapping). Output rendering resizes the finished pixels last.
//!
//! The split lets automatic adjustment evaluate many tone settings on one
//! prepared scene image.

use rayon::prelude::*;
use serde::Serialize;

use crate::adjust::ColorOps;
use crate::arw::RawImage;
use crate::color::{self, srgb_encode, Spaces};
use crate::error::{Error, Result};
use crate::image::{guided_filter, Guided, Image, Plane};
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
    /// Percent of full-active-area 2x2 CFA cells with a sensor-saturated
    /// photosite, before repair or geometry; identical to inspect's statistic.
    pub sensor_clipped_pct: f64,
    /// Pixels per pixel of the full-resolution upright image.
    pub scale: f32,
    pub noise_reduction: crate::noise::NoiseReduction,
    /// `reused`, `stored` or `off`: whether native development was read from
    /// or written to the scene cache. The pixels are identical either way.
    pub scene_reuse: &'static str,
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

fn rotated_dimensions(width: usize, height: usize, degrees: f32) -> (usize, usize) {
    let theta = degrees.to_radians();
    let (sin, cos) = theta.sin_cos();
    let (w, h) = (width as f32, height as f32);
    let (long, short) = (w.max(h), w.min(h));
    let keep = short / (long * sin.abs() + short * cos.abs());
    (((w * keep) as usize).max(1), ((h * keep) as usize).max(1))
}

/// Rotates clockwise about the centre and crops away empty corners.
fn rotated(image: &Image, degrees: f32) -> Image {
    let theta = degrees.to_radians();
    let (sin, cos) = theta.sin_cos();
    let (w, h) = (image.width as f32, image.height as f32);
    let (ow, oh) = rotated_dimensions(image.width, image.height, degrees);
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

fn crop_span(from: f64, to: f64, size: usize) -> (usize, usize) {
    // At least one pixel, and never past the edge, however small the image.
    let start = ((from * size as f64).round() as usize).min(size - 1);
    let end = ((to * size as f64).round() as usize).clamp(start + 1, size);
    (start, end - start)
}

/// Predict native panel dimensions without allocating or developing pixels.
/// Lens correction and finishing retain the active-area dimensions.
pub(crate) fn framed_dimensions(width: usize, height: usize, orientation: u16, params: &Params) -> (usize, usize) {
    let (mut w, mut h) = if matches!(orientation, 6 | 8) { (height, width) } else { (width, height) };
    if params.rotate != 0.0 {
        (w, h) = rotated_dimensions(w, h, params.rotate as f32);
    }
    let crop = &params.crop;
    (crop_span(crop.left, crop.right, w).1, crop_span(crop.top, crop.bottom, h).1)
}

/// Shared rotation and crop geometry, also applied to the camera comparison.
pub(crate) fn framed(image: Image, orientation: u16, params: &Params) -> Image {
    let crop = &params.crop;
    let mut image = image.oriented(orientation);
    if params.rotate != 0.0 {
        image = rotated(&image, params.rotate as f32);
    }
    if *crop != Default::default() {
        let ((x0, cw), (y0, ch)) =
            (crop_span(crop.left, crop.right, image.width), crop_span(crop.top, crop.bottom, image.height));
        image = image.crop(x0, y0, cw, ch);
    }
    image
}

/// Smooths chroma (and, at high strength, luminance) noise. Works on
/// square-root encoded values, where sensor noise is roughly uniform.
///
/// `measured` is the strength that matches this frame's sensor noise. A
/// lower `strength` still removes noise at that level, then restores part of
/// what was removed: fine, even grain. Thresholding at an underestimate of
/// the noise would instead keep only its largest samples, scattered as dark
/// specks and, in the coarse bands, watercolour blotches on fog and water.
fn denoise(image: &mut Image, strength: f32, measured: f32) {
    if strength <= 0.0 || strength >= measured {
        return denoise_at(image, strength);
    }
    let original = image.clone();
    denoise_at(image, measured);
    let kept = strength / measured;
    // Colour grain goes first: none is restored from half the measured strength.
    let (luma_back, chroma_back) = (1.0 - kept, (1.0 - 2.0 * kept).max(0.0));
    let encode = |p: [f32; 3]| {
        let q = p.map(|v| v.max(0.0).sqrt());
        let y = (q[0] + 2.0 * q[1] + q[2]) * 0.25;
        (y, q.map(|v| v - y))
    };
    image.data.par_iter_mut().zip(&original.data).for_each(|(p, o)| {
        let ((y, c), (oy, oc)) = (encode(*p), encode(*o));
        let y = y + (oy - y) * luma_back;
        *p = [0, 1, 2].map(|i| (y + c[i] + (oc[i] - c[i]) * chroma_back).max(0.0).powi(2));
    });
}

fn denoise_at(image: &mut Image, strength: f32) {
    // Low-ISO frames keep their faint grain and finest texture; the
    // luminance threshold reaches its full height by ISO 800 (strength 40).
    let t = ((strength - 0.1) / 0.3).clamp(0.0, 1.0);
    denoise_with(image, strength, 2.2 * t * t * (3.0 - 2.0 * t));
}

/// `luma_threshold` is in units of the luminance noise level; zero smooths
/// chroma only and preserves linear luminance exactly.
fn denoise_with(image: &mut Image, strength: f32, luma_threshold: f32) {
    if strength <= 0.0 {
        return;
    }
    let (w, h) = (image.width, image.height);
    let (mut y, mut original_cr, mut original_cb) = (Plane::new(w, h), Plane::new(w, h), Plane::new(w, h));
    y.data
        .par_iter_mut()
        .zip(original_cr.data.par_iter_mut())
        .zip(original_cb.data.par_iter_mut())
        .zip(&image.data)
        .for_each(|(((y, cr), cb), p)| {
            let p = p.map(|v| v.max(0.0).sqrt());
            *y = (p[0] + 2.0 * p[1] + p[2]) * 0.25;
            *cr = p[0] - p[1];
            *cb = p[2] - p[1];
        });
    let radius = (2.0 + strength * 6.0) as usize;
    let eps = 0.0004 * strength * strength + 1e-6;
    // Chroma follows luminance edges, so colour does not bleed across them.
    let coarse_strength = ((strength - 0.25) / 0.35).clamp(0.0, 1.0);
    let (sw, sh) = (w.div_ceil(4), h.div_ceil(4));
    // Preserve the original guides at quarter size before consuming the
    // native chroma planes; their full-sized copies are no longer needed.
    let guides =
        (coarse_strength > 0.0).then(|| [original_cr.resized(sw, sh), original_cb.resized(sw, sh), y.resized(sw, sh)]);
    let guided = Guided::new(&y, radius);
    let mut cr = guided.filter_owned(original_cr, eps);
    let mut cb = guided.into_filter(original_cb, eps);
    if coarse_strength > 0.0 {
        // A native-pixel filter removes grain but leaves correlated chroma
        // clouds. Estimate that base at quarter size (48-pixel native radius).
        // A chroma boundary mask keeps strong colour discontinuities,
        // including ones with little luminance contrast. Apply only the base
        // correction so fine, luminance-guided colour detail is retained.
        let guides = guides.as_ref().expect("coarse chroma guides");
        let coarse = |chroma: &mut Plane| {
            let small = chroma.resized(sw, sh);
            let base = chroma_base(&small, guides, 12, 0.0064 * strength * strength);
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
    drop(guides);
    // The slider is the noise estimate: 100 stands for a square-root sensor
    // noise of 0.02, of which 0.74 reaches the finest luminance band after
    // demosaicing.
    let luma_sigma = 0.74 * 0.02 * strength;
    // Above half strength the chroma-cleaned estimate also replaces part of
    // the luminance itself.
    let luma_blend = ((strength - 0.5) * 2.0).max(0.0);
    let smooth_luma = luma_threshold > 0.0;
    let original_luma = smooth_luma.then(|| Plane::from_fn(image, |p| color::luma(p).max(0.0).sqrt()));
    image.data.par_iter_mut().enumerate().for_each(|(i, p)| {
        // With chroma-only smoothing, luminance restoration is per-pixel:
        // keep the same sqrt/square round trip without a separate plane/pass.
        let original = if smooth_luma { 0.0 } else { color::luma(*p).max(0.0).sqrt() };
        // Invert the encoded guide, keeping it unchanged by chroma smoothing.
        let g = y.data[i] - (cr.data[i] + cb.data[i]) * 0.25;
        *p = [g + cr.data[i], g, g + cb.data[i]].map(|v| v.max(0.0).powi(2));
        if !smooth_luma {
            let gain = original.max(0.0).powi(2) / color::luma(*p).max(1e-10);
            *p = p.map(|v| v * gain);
        }
    });
    drop(y);
    drop(cr);
    drop(cb);
    // The chroma-cleaned reconstruction is a lower-noise guide. Preserve
    // true linear luminance as the input rather than using sqrt-RGB's proxy.
    if let Some(mut input) = original_luma {
        let guide = Plane::from_fn(image, |p| color::luma(p).max(0.0).sqrt());
        // At high strength, use the chroma-cleaned luminance estimate as well:
        // the camera colour matrix correlates colour noise with noisy Y.
        // Restoring all of that Y would bring the removed noise back as grain.
        input.data.par_iter_mut().zip(&guide.data).for_each(|(v, g)| *v += (*g - *v) * luma_blend);
        drop(guide);
        remove_dark_impulses(&mut input, 3.0 * luma_sigma);
        let luma = crate::image::wavelet_denoise(input, luma_sigma, luma_threshold);
        image.data.par_iter_mut().enumerate().for_each(|(i, p)| {
            let gain = luma.data[i].max(0.0).powi(2) / color::luma(*p).max(1e-10);
            *p = p.map(|v| v * gain);
        });
    }
}

/// Single dark pixels well below all eight neighbours (in square-root
/// luminance) take the median of those neighbours. Wavelet shrinkage
/// leaves such impulses behind as pepper on smooth fog and sky at high
/// ISO. Only dark ones: a bright isolated pixel may be a star or a glint.
fn remove_dark_impulses(plane: &mut Plane, margin: f32) {
    let (w, h) = (plane.width, plane.height);
    if w < 3 || h < 3 || margin <= 0.0 {
        return;
    }
    let source = &plane.data;
    let fixed: Vec<(usize, f32)> = (1..h - 1)
        .into_par_iter()
        .flat_map_iter(|y| {
            (1..w - 1).filter_map(move |x| {
                let v = source[y * w + x];
                let mut around = [0.0f32; 8];
                let mut n = 0;
                for dy in [-1isize, 0, 1] {
                    for dx in [-1isize, 0, 1] {
                        if dx != 0 || dy != 0 {
                            around[n] = source[(y as isize + dy) as usize * w + (x as isize + dx) as usize];
                            n += 1;
                        }
                    }
                }
                let lowest = around.iter().copied().fold(f32::MAX, f32::min);
                if v >= lowest - margin {
                    return None;
                }
                around.sort_unstable_by(f32::total_cmp);
                Some((y * w + x, 0.5 * (around[3] + around[4])))
            })
        })
        .collect();
    for (i, v) in fixed {
        plane.data[i] = v;
    }
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
    pub noise_reduction: crate::noise::NoiseReduction,
    pub scene_reuse: &'static str,
}

/// Develop at native resolution through sharpening, then resize encoded
/// pixels. Nonlinear tone/gamut operations cannot be commuted with averaging;
/// scaling radii alone would still misrepresent noise, clipping and contrast.
pub fn render(raw: &RawImage, params: &Params, size: Size) -> Result<Render> {
    let Scene { image, white_balance, sensor_clipped_pct, noise_reduction, scene_reuse, .. } =
        scene(raw, params, Size::default())?;
    let finished = finish_owned(image, params, &Spaces::new());
    let stage = crate::profile::Stage::new("output resize");
    let image = size.apply(finished);
    drop(stage);
    Ok(Render { image, white_balance, sensor_clipped_pct, noise_reduction, scene_reuse })
}

/// Builds the scene-linear image for a recipe. A reduced size is an
/// approximation for automatic candidate analysis only; use `render` for
/// previews and exports so every spatial/nonlinear stage precedes resizing.
pub fn scene(raw: &RawImage, params: &Params, size: Size) -> Result<Scene> {
    let stage = crate::profile::Stage::new("noise/prepare");
    let noise_reduction = crate::noise::resolve(raw, params.denoise);
    // A lighter explicit setting is applied as a fraction of the measured one.
    let measured = match params.denoise {
        Some(_) => crate::noise::resolve(raw, None).strength,
        None => noise_reduction.strength,
    } as f32;
    let cam_to_work = color::camera_to_working(&camera_matrix(raw)?);
    let (white_balance, gains) = white_balance(raw, params)?;

    // What the final picture's long edge will be at full resolution.
    let crop = &params.crop;
    let (cw, ch) = ((crop.right - crop.left) as f32, (crop.bottom - crop.top) as f32);
    let upright = if matches!(raw.meta.orientation, 6 | 8) {
        (raw.crop.height, raw.crop.width)
    } else {
        (raw.crop.width, raw.crop.height)
    };
    let full_edge = (upright.0 as f32 * cw).max(upright.1 as f32 * ch);
    // Half-resolution demosaicing samples each CFA tile, with automatic
    // neutral-edge colour protection, only for reduced candidate analysis.
    let half = size.max_edge.is_some_and(|edge| edge as f32 <= full_edge * 0.5);

    // Native development depends on the sensor data and these settings only;
    // orientation, rotation and crop follow, so reframing reuses it as well.
    let key = (!half)
        .then(|| {
            crate::cache::key(&crate::cache::Inputs {
                raw,
                gains,
                camera_to_working: cam_to_work,
                lens_corrections: params.lens_corrections,
                defringe: params.defringe,
                denoise: noise_reduction.strength,
            })
        })
        .flatten();
    drop(stage);
    let stage = crate::profile::Stage::new("scene reuse");
    let reused = key.as_ref().and_then(crate::cache::load);
    drop(stage);
    let (image, clipped, scene_reuse) = if let Some(image) = reused {
        (image, crate::stats::sensor_clipping(raw).2, "reused")
    } else {
        let stage = crate::profile::Stage::new("noise/prepare");
        let mosaic = demosaic::prepare(raw, gains);
        drop(stage);
        let stage = crate::profile::Stage::new("demosaic");
        let mut image = if half { demosaic::half(&mosaic) } else { demosaic::full(&mosaic) };
        drop(stage);
        let stage = crate::profile::Stage::new("sensor/highlights");
        let (_, _, clipped) = crate::stats::sensor_clipping(raw);
        demosaic::reconstruct_highlights(&mut image, mosaic.clip);
        let clip = mosaic.clip;
        drop(mosaic);
        drop(stage);
        let stage = crate::profile::Stage::new("lens/matrix/fringe");
        if params.lens_corrections {
            if let Some(tables) = &raw.lens {
                if let Some(corrected) = lens::correct(&image, tables) {
                    image = corrected;
                }
            }
        }
        // After lens correction, which moves red and blue across these edges.
        let scale = |v: usize| if half { v.div_ceil(2) } else { v };
        demosaic::settle_borrowed_light(
            &mut image,
            clip,
            scale(demosaic::BORROWED_SPILL),
            scale(demosaic::BORROWED_REACH),
        );
        image.map(|p| color::apply(&cam_to_work, p));
        crate::fringe::suppress(&mut image, params.defringe as f32 / 100.0, if half { 2 } else { 3 });
        drop(stage);
        let stage = crate::profile::Stage::new("denoise");
        if !half {
            denoise(&mut image, noise_reduction.strength as f32 / 100.0, measured / 100.0);
        }
        drop(stage);
        let stage = crate::profile::Stage::new("scene store");
        let stored = key.as_ref().is_some_and(|key| crate::cache::store(key, &image));
        drop(stage);
        (image, clipped, if stored { "stored" } else { "off" })
    };

    let stage = crate::profile::Stage::new("geometry/scene resize");
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
    drop(stage);
    Ok(Scene { image, white_balance, sensor_clipped_pct: clipped, scale, noise_reduction, scene_reuse })
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
    finish_owned(scene.clone(), params, spaces)
}

/// Global exposure and legacy gradients remain in their original operation order.
fn prepare_local(image: &mut Image, params: &Params) {
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
}

/// Diagnostic mask pixels (encoded grey weights, not tone-mapped or sharpened).
pub fn render_mask(raw: &RawImage, params: &Params, size: Size, index: usize) -> Result<Render> {
    params.validate()?;
    if index >= params.masks.len() {
        return Err(Error::Params(format!(
            "mask index {index} is out of range for {} masks; indices are zero-based",
            params.masks.len()
        )));
    }
    let Scene { mut image, white_balance, sensor_clipped_pct, noise_reduction, scene_reuse, .. } =
        scene(raw, params, Size::default())?;
    prepare_local(&mut image, params);
    let masks = crate::local::Masks::new(&image, params, &Spaces::new());
    let mask = masks.view(index, image.width, image.height);
    Ok(Render { image: size.apply(mask), white_balance, sensor_clipped_pct, noise_reduction, scene_reuse })
}

fn finish_owned(mut image: Image, params: &Params, spaces: &Spaces) -> Image {
    let stage = crate::profile::Stage::new("exposure/local tone");
    prepare_local(&mut image, params);
    let base = BASE_EXPOSURE;
    let masks = crate::local::Masks::new(&image, params, spaces);
    masks.apply_scene(&mut image);
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

    drop(stage);
    let stage = crate::profile::Stage::new("filmic/colour/gamut");
    // Sensor saturation sits at 2^BASE_EXPOSURE and just reaches display white.
    let filmic = Filmic::new(params.contrast as f32, base.exp2());
    // Black and white points on encoded display values: `whites` moves the
    // value that maps to pure white, `blacks` the one that maps to black.
    let black = -params.blacks as f32 / 100.0 * 0.08;
    let white_point = 1.0 - params.whites as f32 / 100.0 * 0.25;
    let curve = (!params.curve.is_empty()).then(|| tone::curve_lut(&params.curve, 1025));
    let ops = ColorOps::new(params, spaces);
    let contrasts = masks.contrast_curves(params);
    let render_colour = |x, y, scene| {
        let mut p = spaces.render_colour(filmic.map_rgb(scene), filmic.log_slope(color::luma(scene)));
        for (n, curve) in &contrasts {
            let weight = masks.weight(*n, x, y);
            if weight > 0.0 {
                let target = spaces.render_colour(curve.map_rgb(scene), curve.log_slope(color::luma(scene)));
                p = [0, 1, 2].map(|c| p[c] + weight * (target[c] - p[c]));
            }
        }
        if black != 0.0 || white_point != 1.0 || curve.is_some() {
            p = p.map(|v| {
                let mut e = srgb_encode(v);
                // Only the low end is clamped here. Light above display
                // white keeps its excess for gamut mapping, as it does
                // without these controls: clamping each channel at 1 here
                // turns a bright warm glow into a hard-edged white disc
                // where its last channel crosses 1.
                e = ((e - black) / (white_point - black)).max(0.0);
                if let Some(lut) = &curve {
                    e = if e <= 1.0 { tone::lut_eval(lut, e) } else { tone::lut_eval(lut, 1.0) + (e - 1.0) };
                }
                color::srgb_decode(e)
            });
        }
        p
    };
    if contrasts.is_empty() {
        image.map(|p| render_colour(0, 0, p));
    } else {
        image.map_xy(render_colour);
    }
    ops.run(&mut image, spaces);
    masks.apply_saturation(&mut image, spaces);
    image.map(|p| spaces.to_srgb_gamut(p).map(srgb_encode));
    drop(stage);
    let stage = crate::profile::Stage::new("sharpen/clamp");
    sharpen(&mut image, params.sharpen as f32 / 100.0 * 1.5);
    image.map(|p| p.map(|v| v.clamp(0.0, 1.0)));
    drop(stage);
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_impulses_go_but_bright_points_and_dark_lines_stay() {
        let mut plane = Plane { width: 9, height: 9, data: vec![0.5; 81] };
        plane.data[2 * 9 + 2] = 0.3; // dark speck
        plane.data[6 * 9 + 6] = 0.9; // star
        for y in 0..9 {
            plane.data[y * 9 + 4] = 0.2; // a one-pixel dark line has dark neighbours
        }
        let before = plane.data.clone();
        remove_dark_impulses(&mut plane, 0.05);
        assert_eq!(plane.data[2 * 9 + 2], 0.5);
        assert_eq!(plane.data[6 * 9 + 6], 0.9);
        for y in 0..9 {
            assert_eq!(plane.data[y * 9 + 4], 0.2);
        }
        let changed = plane.data.iter().zip(&before).filter(|(a, b)| a != b).count();
        assert_eq!(changed, 1);
        // Within the margin nothing moves.
        let mut faint = Plane { width: 3, height: 3, data: vec![0.5; 9] };
        faint.data[4] = 0.47;
        remove_dark_impulses(&mut faint, 0.05);
        assert_eq!(faint.data[4], 0.47);
    }

    #[test]
    fn black_point_and_curve_do_not_draw_an_edge_inside_white_light() {
        // A low sun: a reconstructed neutral core inside a warm glow, both
        // brighter than display white, so the plain render is white across
        // the edge. Black point and curve must not clamp channels one by one
        // before gamut mapping: that drew a hard disc where blue crossed 1.
        let spaces = Spaces::new();
        let w = BASE_EXPOSURE.exp2();
        let mut scene = Image::new(64, 1);
        for (x, p) in scene.data.iter_mut().enumerate() {
            *p = if x < 32 { [1.25 * w, 1.0 * w, 0.62 * w] } else { [3.0 * w; 3] };
        }
        let plain = finish(&scene, &Params::default(), &spaces);
        assert!(plain.at(16, 0).iter().all(|&v| v > 0.99), "{:?}", plain.at(16, 0));
        for recipe in [r#"{"blacks":-15}"#, r#"{"curve":[[0.1,0.07],[0.35,0.33]]}"#, r#"{"blacks":-30,"whites":10}"#] {
            let out = finish(&scene, &Params::from_json(recipe).unwrap(), &spaces);
            let (glow, core) = (out.at(16, 0), out.at(48, 0));
            let step = (0..3).map(|c| (glow[c] - core[c]).abs()).fold(0.0, f32::max);
            assert!(step < 0.01, "{recipe}: {glow:?} against {core:?}");
        }
    }

    #[test]
    fn denoise_preserves_perfectly_flat_image_at_epsilon_regulariser() {
        let value = 0.661_649_1_f32.powi(2);
        let params = Params { denoise: Some(52.44140625), ..Params::default() };
        let strength = params.denoise.unwrap() as f32 / 100.0;
        for (w, h) in [(64, 48), (1, 1), (1, 7), (7, 1), (3, 2)] {
            let mut image = Image { width: w, height: h, data: vec![[value; 3]; w * h] };
            denoise(&mut image, strength, strength);
            for pixel in &image.data {
                assert!(pixel.iter().all(|v| v.is_finite() && (*v - value).abs() < 1e-5), "{w}x{h}: {pixel:?}");
            }
        }
    }

    #[test]
    fn lighter_than_measured_denoise_leaves_even_grain_not_specks() {
        // Flat fog with sensor-like grain, measured at strength 0.6.
        let (w, h, measured) = (192, 160, 0.6f32);
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut normal = || {
            (0..12)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state >> 40) as f32 / (1u64 << 24) as f32
                })
                .sum::<f32>()
                - 6.0
        };
        let noise = 0.02 * measured;
        let mut original = Image::new(w, h);
        for p in &mut original.data {
            *p = [0.3, 0.32, 0.34].map(|v: f32| (v.sqrt() + noise * normal()).max(0.0).powi(2));
        }
        let luma = |image: &Image| -> Vec<f32> {
            (24..h - 24)
                .flat_map(|y| (24..w - 24).map(move |x| (y, x)))
                .map(|(y, x)| image.at(x, y)[1].sqrt())
                .collect()
        };
        // Spread and tail weight of the remaining grain (3 for Gaussian noise).
        let grain = |image: &Image| {
            let v = luma(image);
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            let m2 = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32;
            let m4 = v.iter().map(|x| (x - mean).powi(4)).sum::<f32>() / v.len() as f32;
            (m2.sqrt(), m4 / (m2 * m2))
        };
        let (before, _) = grain(&original);
        let mut previous = before;
        for strength in [0.2, 0.3, 0.45] {
            let mut lighter = original.clone();
            denoise(&mut lighter, strength, measured);
            let mut underestimated = original.clone();
            denoise(&mut underestimated, strength, strength);
            let ((spread, tail), (_, sparse_tail)) = (grain(&lighter), grain(&underestimated));
            eprintln!("strength {strength}: grain {spread:.5} (was {before:.5}), tail {tail:.2}; thresholded alone {sparse_tail:.2}");
            assert!(spread < previous, "less grain at higher strength");
            assert!(tail < 4.0, "even grain: {tail}");
            // The scene must expose the failure: an underestimated threshold
            // keeps only the noise extremes, a heavy tail of isolated specks.
            assert!(strength < 0.4 || sparse_tail > 10.0, "sparse survivors: {sparse_tail}");
            previous = spread;
        }
        let mut full = original.clone();
        denoise(&mut full, measured, measured);
        let mut direct = original.clone();
        denoise_at(&mut direct, measured);
        assert_eq!(full.data, direct.data);
    }

    #[test]
    fn synthetic_scenes_render_finite_at_multiple_denoise_settings() {
        use crate::synthetic::Synthetic;

        // Flat, fully clipped, shadow, skin, haze, snow and saturated sky
        // scenes cover both prior-based and no-survivor highlight recovery.
        for (w, h) in [(16, 16), (66, 50)] {
            for scene_index in 0..8 {
                let synthetic = Synthetic::from_camera_rgb(w, h, |x, y| {
                    let u = x as f32 / (w - 1) as f32;
                    let v = y as f32 / (h - 1) as f32;
                    let grain = ((x * 31 + y * 47) % 97) as f32 / 97.0;
                    match scene_index {
                        0 => [0.0; 3],
                        1 => [0.437_779_55; 3],
                        2 => [1.0; 3],
                        3 => [0.0001 + grain * 0.001, 0.001 + grain * 0.003, 0.0002 + grain * 0.002],
                        4 => [0.25 + u * 0.4, 0.12 + v * 0.2, 0.08 + grain * 0.01],
                        5 => [0.18 + u * 0.06, 0.22 + v * 0.04, 0.28 + grain * 0.005],
                        6 => [0.5 + u * 0.7, 0.6 + u * 0.7, 0.7 + u * 0.7],
                        _ => [0.1 + u * 1.4, 0.18 + v * 1.1, 0.7 + u * 0.6],
                    }
                });
                let raw = crate::arw::decode(&synthetic.to_bytes()).unwrap();
                for denoise in [0.0, 0.00001, 25.00001, 50.0, 50.00001, 52.44140625, 85.0, 100.0] {
                    let params = Params { denoise: Some(denoise), ..Params::default() };
                    params.validate().unwrap();
                    let context = format!("scene {scene_index}, {w}x{h}, denoise {denoise}");
                    // Check before finishing too: later max/clamp operations
                    // can hide a non-finite scene as a successful black render.
                    let scene = scene(&raw, &params, Size::default()).unwrap();
                    assert!(scene.image.data.iter().flatten().all(|v| v.is_finite()), "{context}: scene");
                    for max_edge in [None, Some(1), Some(23)] {
                        let rendered = render(&raw, &params, Size { max_edge }).unwrap();
                        assert!(rendered.image.data.iter().flatten().all(|v| v.is_finite()), "{context}: {max_edge:?}");
                    }
                }
            }
        }
    }

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
        denoise_with(&mut image, 0.5, 0.0);
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
            denoise(&mut image, strength, strength);
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

#[cfg(test)]
#[path = "legacy_local_tests.rs"]
mod legacy_local_tests;

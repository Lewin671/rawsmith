//! Tone: exposure, local shadow/highlight compression, the filmic tone
//! curve from scene-linear light to display-linear, and display-referred
//! white/black points and curve.

use rayon::prelude::*;

use crate::color::luma;
use crate::image::{Image, Plane};

/// Sigmoid exponent at zero slider contrast. With the native white point,
/// this gives ~1.5 log-log slope at middle grey, with a toe reaching true
/// black rather than a display offset that would crush shadow detail.
const BASE_CONTRAST: f32 = 1.8;

/// Scene-linear middle grey.
pub const GREY: f32 = 0.18;

/// Fraction of the long edge used as the radius that separates "local" from
/// "global" brightness.
const BASE_RADIUS: f32 = 0.035;
/// Long edge of the reduced image the base layer is estimated on.
const BASE_SIZE: usize = 720;

/// Splits log2 luminance into a smooth, edge-aware base layer and returns the
/// base. Linear coefficients are estimated on a reduced copy and applied to
/// the full-resolution luminance (fast guided filter), so edges stay sharp.
pub fn base_layer(log_luma: &Plane) -> Plane {
    let (w, h) = (log_luma.width, log_luma.height);
    let scale = (BASE_SIZE as f32 / w.max(h) as f32).min(1.0);
    let (sw, sh) = (((w as f32 * scale) as usize).max(1), ((h as f32 * scale) as usize).max(1));
    let small = log_luma.resized(sw, sh);
    let radius = ((sw.max(sh) as f32 * BASE_RADIUS) as usize).max(1);

    let mean = small.box_blur(radius);
    let squares = Plane { width: sw, height: sh, data: small.data.iter().map(|v| v * v).collect() };
    let mean_sq = squares.box_blur(radius);
    // eps is in squared stops: edges larger than ~1 stop are preserved.
    let eps = 1.0;
    let mut a = Plane::new(sw, sh);
    let mut b = Plane::new(sw, sh);
    for i in 0..sw * sh {
        let var = (mean_sq.data[i] - mean.data[i] * mean.data[i]).max(0.0);
        a.data[i] = var / (var + eps);
        b.data[i] = mean.data[i] * (1.0 - a.data[i]);
    }
    let (a, b) = (a.box_blur(radius), b.box_blur(radius));

    let mut base = Plane::new(w, h);
    let (fx, fy) = (sw as f32 / w as f32, sh as f32 / h as f32);
    base.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = (y as f32 + 0.5) * fy - 0.5;
        for (x, out) in row.iter_mut().enumerate() {
            let sx = (x as f32 + 0.5) * fx - 0.5;
            *out = a.sample(sx, sy) * log_luma.data[y * w + x] + b.sample(sx, sy);
        }
    });
    base
}

/// Smooth one-sided ramp: 0 for `d <= 0`, approaching `d - knee` for large `d`.
#[inline]
fn ramp(d: f32, knee: f32) -> f32 {
    if d <= 0.0 {
        0.0
    } else {
        d * d / (d + knee)
    }
}

pub struct Local {
    /// -1..1: negative darkens bright regions.
    pub highlights: f32,
    /// -1..1: positive brightens dark regions.
    pub shadows: f32,
    /// -1..1: local contrast.
    pub clarity: f32,
}

/// Applies local tone adjustments to a scene-linear image in place.
pub fn local_tone(image: &mut Image, local: &Local) {
    if local.highlights == 0.0 && local.shadows == 0.0 && local.clarity == 0.0 {
        return;
    }
    let log_luma = Plane::from_fn(image, |p| luma(p).max(1e-6).log2());
    let base = base_layer(&log_luma);
    let pivot = GREY.log2();
    // At full strength a region two stops from middle grey moves by about
    // 1.5 stops, and local detail contrast doubles (or vanishes).
    let (kh, ks, kc) = (local.highlights, local.shadows, local.clarity);
    image.data.par_iter_mut().enumerate().for_each(|(i, p)| {
        let d = base.data[i] - pivot;
        let detail = log_luma.data[i] - base.data[i];
        let stops = kh * ramp(d, 0.7) + ks * ramp(-d, 0.7) + kc * detail;
        let gain = stops.exp2();
        *p = [p[0] * gain, p[1] * gain, p[2] * gain];
    });
}

/// Separable minimum filter (erosion) with the given radius.
fn erode(plane: &Plane, radius: usize) -> Plane {
    let (w, h) = (plane.width, plane.height);
    let mut mid = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(radius), (x + radius).min(w - 1));
            mid.data[y * w + x] = plane.data[y * w + a..=y * w + b].iter().copied().fold(f32::MAX, f32::min);
        }
    }
    let mut out = Plane::new(w, h);
    for y in 0..h {
        let (a, b) = (y.saturating_sub(radius), (y + radius).min(h - 1));
        for x in 0..w {
            out.data[y * w + x] = (a..=b).map(|yy| mid.data[yy * w + x]).fold(f32::MAX, f32::min);
        }
    }
    out
}

/// Mean log luminance with a fixed addition order, independent of Rayon's
/// task splits. Keep the expensive logarithms parallel; only the small list
/// of block totals is merged serially. f64 avoids drift on large images.
fn mean_log_luma(image: &Image) -> f32 {
    let sums: Vec<f64> =
        image.data.par_chunks(4096).map(|block| block.iter().map(|p| luma(*p).max(1e-5).ln() as f64).sum()).collect();
    (sums.iter().sum::<f64>() / image.data.len() as f64) as f32
}

/// Removes atmospheric haze (dark channel prior, He et al.): haze adds a
/// veil of airlight that lifts the darkest channel everywhere; estimating
/// that veil and subtracting it restores contrast and colour with distance.
/// `strength` is 0..1.
pub fn dehaze(image: &mut Image, strength: f32) {
    if strength <= 0.0 {
        return;
    }
    let (w, h) = (image.width, image.height);
    let scale = (BASE_SIZE as f32 / w.max(h) as f32).min(1.0);
    let (sw, sh) = (((w as f32 * scale) as usize).max(1), ((h as f32 * scale) as usize).max(1));
    let small = image.resized(sw, sh);
    let radius = (sw.max(sh) / 60).max(1);

    // Airlight: the colour of the haziest pixels (largest dark channel).
    let dark = erode(&Plane::from_fn(&small, |p| p[0].min(p[1]).min(p[2])), radius);
    let mut order: Vec<usize> = (0..dark.data.len()).collect();
    order.sort_unstable_by(|&a, &b| dark.data[b].total_cmp(&dark.data[a]));
    let top = &order[..(order.len() / 500).max(1)];
    let mut air = [0.0f32; 3];
    for &i in top {
        for c in 0..3 {
            air[c] += small.data[i][c] / top.len() as f32;
        }
    }
    let air = air.map(|v| v.max(1e-3));

    // Transmission: how much of the scene (rather than airlight) each pixel shows.
    let normalised = Plane::from_fn(&small, |p| (p[0] / air[0]).min(p[1] / air[1]).min(p[2] / air[2]));
    let rough = erode(&normalised, radius);
    let rough =
        Plane { width: sw, height: sh, data: rough.data.iter().map(|d| 1.0 - 0.9 * strength * d.min(1.0)).collect() };
    let guide = Plane::from_fn(&small, |p| luma(p).max(0.0).sqrt());
    let transmission = crate::image::guided_filter(&guide, &rough, radius * 4, 1e-3);

    let floor = 1.0 - 0.9 * strength;
    let (fx, fy) = (sw as f32 / w as f32, sh as f32 / h as f32);
    let before = mean_log_luma(image);
    image.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = (y as f32 + 0.5) * fy - 0.5;
        for (x, p) in row.iter_mut().enumerate() {
            // The edge-aware refinement can undershoot; never remove more of
            // the veil than the strength allows.
            let t = transmission.sample((x as f32 + 0.5) * fx - 0.5, sy).clamp(floor, 1.0);
            for c in 0..3 {
                p[c] = ((p[c] - air[c]) / t + air[c]).max(0.0);
            }
        }
    });
    // Subtracting the veil darkens the picture; restore its overall
    // (geometric mean) brightness so dehaze changes contrast, not exposure.
    let gain = (before - mean_log_luma(image)).exp().clamp(1.0, 2.0);
    image.map(|p| p.map(|v| v * gain));
}

/// The filmic curve: a log-logistic sigmoid that maps scene-linear values to
/// display-linear 0..1. Middle grey stays at middle grey, `white` (scene
/// value) reaches display white, shadows roll into a toe and highlights into
/// a shoulder.
#[derive(Clone, Copy)]
pub struct Filmic {
    contrast: f32,
    pivot: f32,
    scale: f32,
    white: f32,
    shoulder: f32,
    shoulder_slope: f32,
}

impl Filmic {
    /// `contrast` is the slider value -100..100; `white` the scene-linear
    /// value that should just reach display white.
    pub fn new(contrast: f32, white: f32) -> Self {
        let c = BASE_CONTRAST * (contrast / 100.0 * 0.5).exp2();
        let white = white.max(GREY * 2.0);
        // Solve scale * g^c / (g^c + p) = g and scale * w^c / (w^c + p) = 1.
        let (gc, wc) = (GREY.powf(c), white.powf(c));
        let p = gc * wc * (1.0 - GREY) / (GREY * wc - gc);
        // A white point too close to grey for this contrast has no solution;
        // fall back to an unscaled curve.
        let (pivot, scale) = if p > 0.0 { (p, (wc + p) / wc) } else { (gc * (1.0 - GREY) / GREY, 1.0) };
        let knee = white * 0.5;
        let kc = knee.powf(c);
        let shoulder = scale * kc / (kc + pivot);
        let shoulder_slope = scale * c * pivot * kc / (knee * (kc + pivot).powi(2));
        Filmic { contrast: c, pivot, scale, white, shoulder, shoulder_slope }
    }

    /// Local log-log contrast. Near neutral, this is the relative colour
    /// separation a channel-wise curve would produce, without its hue shifts.
    pub fn log_slope(&self, x: f32) -> f32 {
        if x <= 0.0 || x >= self.white {
            return 0.0;
        }
        let knee = self.white * 0.5;
        if x <= knee {
            return self.contrast * self.pivot / (x.powf(self.contrast) + self.pivot);
        }
        let t = (x - knee) / knee;
        let d = (1.0 - self.shoulder).max(0.0);
        let m = (self.shoulder_slope * knee).min(3.0 * d);
        let derivative = (2.0 * (1.0 - t) * (d + (2.0 * d - m) * t) - (1.0 - t).powi(2) * (2.0 * d - m)) / knee;
        (x * derivative / self.eval(x)).max(0.0)
    }

    /// Apply the curve to luminance with one RGB gain. Channel-wise curves
    /// wash out shoulder colours and rotate their hue. A common gain keeps
    /// chromaticity (and Oklab C/L) until the output gamut requires compression.
    /// Preserve negative matrix channels too; clipping them here changes hue.
    #[inline]
    pub fn map_rgb(&self, rgb: [f32; 3]) -> [f32; 3] {
        let y = luma(rgb);
        if y <= 0.0 {
            return [0.0; 3];
        }
        let gain = self.eval(y) / y;
        rgb.map(|v| v * gain)
    }

    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        let knee = self.white * 0.5;
        if x > knee {
            // Hermite shoulder: match the sigmoid at the knee, then reach
            // white with zero derivative. No abrupt per-channel flat cut-off.
            let t = ((x - knee) / knee).min(1.0);
            let d = (1.0 - self.shoulder).max(0.0);
            let m = (self.shoulder_slope * knee).min(3.0 * d);
            return 1.0 - (1.0 - t).powi(2) * (d + (2.0 * d - m) * t);
        }
        let xc = x.powf(self.contrast);
        (self.scale * xc / (xc + self.pivot)).min(1.0)
    }
}

/// Monotone cubic (Fritsch-Carlson) interpolation through control points, as
/// a lookup table over 0..1.
pub fn curve_lut(points: &[[f64; 2]], size: usize) -> Vec<f32> {
    // The ends of the range stay put unless the caller moves them.
    let mut points = points.to_vec();
    if points.first().is_none_or(|p| p[0] > 0.0) {
        points.insert(0, [0.0, 0.0]);
    }
    if points.last().is_some_and(|p| p[0] < 1.0) {
        points.push([1.0, 1.0]);
    }
    let n = points.len();
    let xs: Vec<f64> = points.iter().map(|p| p[0]).collect();
    let ys: Vec<f64> = points.iter().map(|p| p[1]).collect();
    let slopes: Vec<f64> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i])).collect();
    let mut tangents = vec![0.0; n];
    for i in 0..n {
        tangents[i] = if i == 0 {
            slopes[0]
        } else if i == n - 1 {
            slopes[n - 2]
        } else if slopes[i - 1] * slopes[i] <= 0.0 {
            0.0
        } else {
            // Harmonic mean keeps the interpolant monotone.
            2.0 * slopes[i - 1] * slopes[i] / (slopes[i - 1] + slopes[i])
        };
    }
    (0..size)
        .map(|k| {
            let x = k as f64 / (size - 1) as f64;
            if x <= xs[0] {
                return ys[0] as f32;
            }
            if x >= xs[n - 1] {
                return ys[n - 1] as f32;
            }
            let i = xs.partition_point(|&v| v <= x) - 1;
            let h = xs[i + 1] - xs[i];
            let t = (x - xs[i]) / h;
            let (t2, t3) = (t * t, t * t * t);
            let y = (2.0 * t3 - 3.0 * t2 + 1.0) * ys[i]
                + (t3 - 2.0 * t2 + t) * h * tangents[i]
                + (-2.0 * t3 + 3.0 * t2) * ys[i + 1]
                + (t3 - t2) * h * tangents[i + 1];
            y.clamp(0.0, 1.0) as f32
        })
        .collect()
}

#[inline]
pub fn lut_eval(lut: &[f32], v: f32) -> f32 {
    let pos = v.clamp(0.0, 1.0) * (lut.len() - 1) as f32;
    let i = (pos as usize).min(lut.len() - 2);
    lut[i] + (lut[i + 1] - lut[i]) * (pos - i as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filmic_anchors_grey_and_white_and_is_monotone() {
        for contrast in [-100.0, 0.0, 100.0] {
            for white in [1.0, 4.0, 16.0] {
                let f = Filmic::new(contrast, white);
                assert!((f.eval(GREY) - GREY).abs() < 1e-3, "grey moved at contrast {contrast}, white {white}");
                assert!((f.eval(white) - 1.0).abs() < 1e-3);
                let mut last = 0.0;
                for i in 1..2000 {
                    let y = f.eval(i as f32 * white / 1000.0);
                    assert!(y >= last && y <= 1.0);
                    last = y;
                }
            }
        }
    }

    #[test]
    fn filmic_reaches_white_with_a_flat_shoulder() {
        for contrast in [-100.0, 0.0, 100.0] {
            let f = Filmic::new(contrast, 2.3);
            let step = 0.0001;
            let slope = (f.eval(2.3) - f.eval(2.3 - step)) / step;
            assert!(slope.abs() < 0.002, "hard shoulder at contrast {contrast}: {slope}");
        }
    }

    #[test]
    fn curve_lut_interpolates_points_monotonically() {
        let lut = curve_lut(&[[0.0, 0.0], [0.25, 0.4], [0.75, 0.8], [1.0, 1.0]], 1025);
        assert!((lut_eval(&lut, 0.25) - 0.4).abs() < 1e-3);
        assert!((lut_eval(&lut, 0.75) - 0.8).abs() < 1e-3);
        assert!(lut.windows(2).all(|w| w[1] >= w[0]));
        // Missing end points are implied, not clamped to the nearest point.
        let lifted = curve_lut(&[[0.15, 0.22], [0.5, 0.5]], 1025);
        assert!(lut_eval(&lifted, 0.0) < 1e-4 && (lut_eval(&lifted, 1.0) - 1.0).abs() < 1e-4);
        assert!((lut_eval(&lifted, 0.75) - 0.75).abs() < 0.03);
        let identity = curve_lut(&[[0.0, 0.0], [1.0, 1.0]], 257);
        assert!((lut_eval(&identity, 0.3) - 0.3).abs() < 1e-4);
    }

    #[test]
    fn base_layer_keeps_a_step_edge_and_flattens_texture() {
        // Two flat halves 3 stops apart with fine texture on top.
        let (w, h) = (400, 300);
        let mut plane = Plane::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let level = if x < w / 2 { -4.0 } else { -1.0 };
                plane.data[y * w + x] = level + 0.1 * (((x + y) % 2) as f32 - 0.5);
            }
        }
        let base = base_layer(&plane);
        let at = |x: usize| base.data[150 * w + x];
        assert!((at(60) + 4.0).abs() < 0.15 && (at(340) + 1.0).abs() < 0.15);
        // The edge stays within a few pixels instead of spreading over the radius.
        assert!(at(190) < -3.3 && at(210) > -1.7, "{} {}", at(190), at(210));
    }
}

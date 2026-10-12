//! Colour adjustments on the tone-mapped image, done in Oklab so hue,
//! chroma and lightness can be changed independently.

use crate::color::{apply, srgb_decode, Spaces};
use crate::image::Image;
use crate::params::{Band, Grade, Params, Tint};

/// Oklch hue (degrees) at the centre of each HSL band, in `HSL_BANDS` order.
pub const BAND_HUES: [f32; 8] = [29.0, 58.0, 105.0, 142.0, 195.0, 258.0, 300.0, 335.0];
/// Hue shift, in degrees, of a band at hue = 100.
const HUE_RANGE: f32 = 30.0;
/// Oklab lightness change of a fully saturated colour at luminance = 100.
const LUMINANCE_RANGE: f32 = 0.25;
/// Chroma below which a colour counts as nearly neutral.
const NEUTRAL_CHROMA: f32 = 0.04;
/// Oklab offset of a colour-grade tint at saturation = 100.
const GRADE_RANGE: f32 = 0.08;

#[inline]
fn smoothstep(lo: f32, hi: f32, v: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The two bands a hue falls between and the weight of the second.
#[inline]
pub fn band_weights(hue: f32) -> (usize, usize, f32) {
    let hue = hue.rem_euclid(360.0);
    for i in 0..8 {
        let next = (i + 1) % 8;
        let start = BAND_HUES[i];
        let span = (BAND_HUES[next] - start).rem_euclid(360.0);
        let offset = (hue - start).rem_euclid(360.0);
        if offset < span {
            return (i, next, smoothstep(0.0, 1.0, offset / span));
        }
    }
    (0, 0, 0.0)
}

/// Oklab direction of a colour-wheel hue (0 red, 120 green, 240 blue).
fn wheel_direction(spaces: &Spaces, hue: f32) -> [f32; 2] {
    let h = hue.rem_euclid(360.0) / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let srgb = match h as u32 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    };
    let lab = spaces.to_oklab(apply(&spaces.srgb_to_work, srgb.map(srgb_decode)));
    let len = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt().max(1e-6);
    [lab[1] / len, lab[2] / len]
}

pub struct ColorOps {
    vibrance: f32,
    saturation: f32,
    bands: Option<[Band; 8]>,
    shadows: Option<[f32; 2]>,
    highlights: Option<[f32; 2]>,
    balance: f32,
}

impl ColorOps {
    pub fn new(params: &Params, spaces: &Spaces) -> Self {
        let tint = |t: &Tint| {
            (t.saturation > 0.0).then(|| {
                let d = wheel_direction(spaces, t.hue as f32);
                let k = t.saturation as f32 / 100.0 * GRADE_RANGE;
                [d[0] * k, d[1] * k]
            })
        };
        let Grade { shadows, highlights, balance } = &params.grade;
        ColorOps {
            vibrance: params.vibrance as f32 / 100.0,
            saturation: params.saturation as f32 / 100.0,
            bands: (params.hsl != Default::default()).then(|| params.hsl.bands()),
            shadows: tint(shadows),
            highlights: tint(highlights),
            balance: *balance as f32 / 100.0 * 0.3,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.vibrance == 0.0
            && self.saturation == 0.0
            && self.bands.is_none()
            && self.shadows.is_none()
            && self.highlights.is_none()
    }

    #[inline]
    fn apply_lab(&self, lab: [f32; 3]) -> [f32; 3] {
        let [mut l, mut a, mut b] = lab;
        let mut chroma = (a * a + b * b).sqrt();
        let mut hue = b.atan2(a).to_degrees();

        if let Some(bands) = &self.bands {
            let (i, j, t) = band_weights(hue);
            let mix = |f: fn(&Band) -> f64| (f(&bands[i]) as f32 * (1.0 - t) + f(&bands[j]) as f32 * t) / 100.0;
            // Near-neutral colours have no meaningful hue; fade the band out.
            let colourful = smoothstep(0.0, NEUTRAL_CHROMA, chroma);
            hue += mix(|b| b.hue) * HUE_RANGE * colourful;
            chroma *= (1.0 + mix(|b| b.saturation) * colourful).max(0.0);
            l += mix(|b| b.luminance) * LUMINANCE_RANGE * smoothstep(0.0, 0.12, chroma);
        }

        let mut gain = 1.0 + self.saturation;
        if self.vibrance != 0.0 {
            // Muted colours get the full change, vivid ones little; skin
            // hues (around orange) get half.
            let muted = 1.0 - smoothstep(0.05, 0.28, chroma);
            let skin = 1.0 - 0.5 * (1.0 - smoothstep(0.0, 30.0, (hue - 55.0).abs()));
            gain *= 1.0 + self.vibrance * if self.vibrance > 0.0 { muted * skin } else { 1.0 };
        }
        chroma *= gain.max(0.0);

        let (sin, cos) = hue.to_radians().sin_cos();
        a = chroma * cos;
        b = chroma * sin;

        if self.shadows.is_some() || self.highlights.is_some() {
            let split = 0.55 - self.balance;
            if let Some(t) = self.shadows {
                // No tint at pure black, where it would only lift the blacks' colour noise.
                let w = (1.0 - smoothstep(split - 0.35, split + 0.15, l)) * smoothstep(0.0, 0.12, l);
                a += t[0] * w;
                b += t[1] * w;
            }
            if let Some(t) = self.highlights {
                let w = smoothstep(split - 0.15, split + 0.35, l) * (1.0 - smoothstep(0.92, 1.0, l));
                a += t[0] * w;
                b += t[1] * w;
            }
        }
        [l.clamp(0.0, 1.0), a, b]
    }

    /// Applies the adjustments to a display-linear working-space image.
    pub fn run(&self, image: &mut Image, spaces: &Spaces) {
        if self.is_identity() {
            return;
        }
        image.map(|p| spaces.from_oklab(self.apply_lab(spaces.to_oklab(p))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(edit: impl Fn(&mut Params)) -> (ColorOps, Spaces) {
        let spaces = Spaces::new();
        let mut params = Params::default();
        edit(&mut params);
        (ColorOps::new(&params, &spaces), spaces)
    }

    fn chroma(lab: [f32; 3]) -> f32 {
        (lab[1] * lab[1] + lab[2] * lab[2]).sqrt()
    }

    #[test]
    fn band_weights_peak_at_centres_and_wrap() {
        for (i, &h) in BAND_HUES.iter().enumerate() {
            let (a, _, t) = band_weights(h);
            assert_eq!((a, t), (i, 0.0));
        }
        let (a, b, t) = band_weights(10.0);
        assert_eq!((a, b), (7, 0));
        assert!(t > 0.5);
    }

    #[test]
    fn defaults_are_identity_and_saturation_minus_100_is_grey() {
        assert!(ops(|_| {}).0.is_identity());
        let (o, _) = ops(|p| p.saturation = -100.0);
        assert!(chroma(o.apply_lab([0.6, 0.1, -0.05])) < 1e-6);
    }

    #[test]
    fn hsl_band_only_touches_its_hue_range() {
        let (o, _) = ops(|p| p.hsl.blue.saturation = -80.0);
        let lab_at = |deg: f32| [0.6, 0.12 * deg.to_radians().cos(), 0.12 * deg.to_radians().sin()];
        assert!(chroma(o.apply_lab(lab_at(258.0))) < 0.03);
        assert!((chroma(o.apply_lab(lab_at(58.0))) - 0.12).abs() < 1e-4);
        assert!((chroma(o.apply_lab(lab_at(142.0))) - 0.12).abs() < 1e-4);
    }

    #[test]
    fn vibrance_favours_muted_colours() {
        let (o, _) = ops(|p| p.vibrance = 60.0);
        let muted = chroma(o.apply_lab([0.6, -0.04, -0.04])) / chroma([0.6, -0.04, -0.04]);
        let vivid = chroma(o.apply_lab([0.6, -0.2, -0.2])) / chroma([0.6, -0.2, -0.2]);
        assert!(muted > 1.4 && vivid < 1.15, "{muted} {vivid}");
    }

    #[test]
    fn grade_tints_shadows_towards_the_requested_hue() {
        let (o, _) = ops(|p| p.grade.shadows = Tint { hue: 240.0, saturation: 50.0 });
        let dark = o.apply_lab([0.3, 0.0, 0.0]);
        let bright = o.apply_lab([0.9, 0.0, 0.0]);
        // Blue lies at negative b in Oklab.
        assert!(dark[2] < -0.02 && chroma(bright) < 1e-3, "{dark:?} {bright:?}");
    }
}

//! Colour science: camera matrices, white balance from temperature and tint,
//! the working space, Oklab and output encoding.
//!
//! The working space is linear Rec.2020 (D65): wide enough that saturated
//! sunsets do not clip before tone mapping.

pub type Mat3 = [[f32; 3]; 3];

pub const REC2020_TO_XYZ: Mat3 =
    [[0.636_958, 0.144_616_9, 0.168_881], [0.262_700_2, 0.677_998_1, 0.059_301_7], [0.0, 0.028_072_7, 1.060_985_1]];

pub const SRGB_TO_XYZ: Mat3 = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175],
    [0.019_333_9, 0.119_192, 0.950_304_1],
];

/// Rec.2020 luminance weights.
pub const LUMA: [f32; 3] = [0.262_700_2, 0.677_998_1, 0.059_301_7];

#[inline]
pub fn luma(p: [f32; 3]) -> f32 {
    p[0] * LUMA[0] + p[1] * LUMA[1] + p[2] * LUMA[2]
}

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

#[inline]
pub fn apply(m: &Mat3, v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

pub fn invert(m: &Mat3) -> Mat3 {
    let m64 = m.map(|row| row.map(|v| v as f64));
    let cofactor = |r: usize, c: usize| {
        let (r1, r2, c1, c2) = ((r + 1) % 3, (r + 2) % 3, (c + 1) % 3, (c + 2) % 3);
        m64[r1][c1] * m64[r2][c2] - m64[r1][c2] * m64[r2][c1]
    };
    let det: f64 = (0..3).map(|c| m64[0][c] * cofactor(0, c)).sum();
    let mut out = [[0.0f32; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[c][r] = (cofactor(r, c) / det) as f32;
        }
    }
    out
}

/// Matrices that map CIE XYZ (D65) to camera RGB, per camera model. These
/// are sensor characterisation data; a model missing here cannot be rendered
/// with correct colour.
const CAMERAS: &[(&str, [i32; 9])] = &[("ILCE-7M4", [7460, -2365, -588, -5687, 13442, 2474, -624, 1156, 6584])];

pub fn camera_matrix(model: &str) -> Option<Mat3> {
    let (_, m) = CAMERAS.iter().find(|(name, _)| *name == model)?;
    let v = |i: usize| m[i] as f32 / 10_000.0;
    Some([[v(0), v(1), v(2)], [v(3), v(4), v(5)], [v(6), v(7), v(8)]])
}

pub fn supported_cameras() -> Vec<&'static str> {
    CAMERAS.iter().map(|(name, _)| *name).collect()
}

/// Matrix from white-balanced camera RGB to the working space. Rows of the
/// forward matrix are normalised so camera neutral maps to working-space
/// neutral: white balance does the chromatic adaptation.
pub fn camera_to_working(xyz_to_cam: &Mat3) -> Mat3 {
    let mut forward = mul(xyz_to_cam, &REC2020_TO_XYZ);
    for row in forward.iter_mut() {
        let sum: f32 = row.iter().sum();
        row.iter_mut().for_each(|v| *v /= sum);
    }
    invert(&forward)
}

/// CIE 1960 uv chromaticity of a black body at `kelvin` (Krystek 1985).
fn planck_uv(kelvin: f64) -> (f64, f64) {
    let t = kelvin;
    let u =
        (0.860117757 + 1.54118254e-4 * t + 1.28641212e-7 * t * t) / (1.0 + 8.42420235e-4 * t + 7.08145163e-7 * t * t);
    let v =
        (0.317398726 + 4.22806245e-5 * t + 4.20481691e-8 * t * t) / (1.0 - 2.89741816e-5 * t + 1.61456053e-7 * t * t);
    (u, v)
}

/// Unit normal of the Planckian locus at `kelvin`, pointing towards green.
fn planck_normal(kelvin: f64) -> (f64, f64) {
    let (u0, v0) = planck_uv(kelvin - 1.0);
    let (u1, v1) = planck_uv(kelvin + 1.0);
    // The locus runs towards lower u as temperature rises; rotating that
    // tangent by +90 degrees points to higher v, the green side.
    let (du, dv) = (u1 - u0, v1 - v0);
    let len = (du * du + dv * dv).sqrt();
    let (nu, nv) = (-dv / len, du / len);
    if nv < 0.0 {
        (-nu, -nv)
    } else {
        (nu, nv)
    }
}

pub const MIN_KELVIN: f64 = 2000.0;
pub const MAX_KELVIN: f64 = 15000.0;
/// Distance from the locus, in uv units, of one tint step. Positive tint is
/// magenta, as in common raw developers.
const TINT_STEP: f64 = 1.0 / 3000.0;

/// White balance multipliers (G = 1) that neutralise an illuminant of the
/// given temperature and tint for a camera.
pub fn wb_from_temperature(xyz_to_cam: &Mat3, kelvin: f64, tint: f64) -> [f32; 3] {
    let kelvin = kelvin.clamp(MIN_KELVIN, MAX_KELVIN);
    let (u, v) = planck_uv(kelvin);
    let (nu, nv) = planck_normal(kelvin);
    let (u, v) = (u - nu * tint * TINT_STEP, v - nv * tint * TINT_STEP);
    let denom = 2.0 * u - 8.0 * v + 4.0;
    let (x, y) = (3.0 * u / denom, 2.0 * v / denom);
    let xyz = [(x / y) as f32, 1.0, ((1.0 - x - y) / y) as f32];
    let cam = apply(xyz_to_cam, xyz);
    [cam[1] / cam[0], 1.0, cam[1] / cam[2]]
}

/// Inverse of [`wb_from_temperature`]: the temperature and tint whose
/// multipliers are closest to `wb`.
pub fn temperature_from_wb(xyz_to_cam: &Mat3, wb: [f32; 3]) -> (f64, f64) {
    let neutral = [1.0 / wb[0], 1.0 / wb[1], 1.0 / wb[2]];
    let xyz = apply(&invert(xyz_to_cam), neutral);
    let sum = (xyz[0] + 15.0 * xyz[1] + 3.0 * xyz[2]) as f64;
    let (u, v) = (4.0 * xyz[0] as f64 / sum, 6.0 * xyz[1] as f64 / sum);
    // The foot of the perpendicular from (u, v) onto the locus: the signed
    // tangential offset changes sign there, so bisect on it.
    let along = |kelvin: f64| {
        let (pu, pv) = planck_uv(kelvin);
        let (nu, nv) = planck_normal(kelvin);
        (u - pu) * nv - (v - pv) * nu
    };
    let (mut lo, mut hi) = (MIN_KELVIN, MAX_KELVIN);
    let rising = along(lo) < along(hi);
    for _ in 0..50 {
        let mid = 0.5 * (lo + hi);
        if (along(mid) < 0.0) == rising {
            lo = mid
        } else {
            hi = mid
        }
    }
    let kelvin = 0.5 * (lo + hi);
    let (pu, pv) = planck_uv(kelvin);
    let (nu, nv) = planck_normal(kelvin);
    let tint = -((u - pu) * nu + (v - pv) * nv) / TINT_STEP;
    (kelvin, tint)
}

// --- Oklab (Björn Ottosson), from the linear Rec.2020 working space ---------

const XYZ_TO_LMS: Mat3 = [
    [0.818_933, 0.361_866_74, -0.128_859_71],
    [0.032_984_544, 0.929_311_9, 0.036_145_64],
    [0.048_200_3, 0.264_366_27, 0.633_851_7],
];
const LMS_TO_LAB: Mat3 = [
    [0.210_454_26, 0.793_617_8, -0.004_072_047],
    [1.977_998_5, -2.428_592_2, 0.450_593_7],
    [0.025_904_037, 0.782_771_77, -0.808_675_77],
];

/// Precomputed conversions between the working space, Oklab and sRGB.
pub struct Spaces {
    work_to_lms: Mat3,
    lms_to_work: Mat3,
    lab_to_lms: Mat3,
    pub work_to_srgb: Mat3,
    pub srgb_to_work: Mat3,
}

impl Spaces {
    pub fn new() -> Self {
        let work_to_lms = mul(&XYZ_TO_LMS, &REC2020_TO_XYZ);
        let work_to_srgb = mul(&invert(&SRGB_TO_XYZ), &REC2020_TO_XYZ);
        Spaces {
            lms_to_work: invert(&work_to_lms),
            work_to_lms,
            lab_to_lms: invert(&LMS_TO_LAB),
            srgb_to_work: invert(&work_to_srgb),
            work_to_srgb,
        }
    }

    #[inline]
    pub fn to_oklab(&self, rgb: [f32; 3]) -> [f32; 3] {
        let lms = apply(&self.work_to_lms, rgb);
        apply(&LMS_TO_LAB, [lms[0].cbrt(), lms[1].cbrt(), lms[2].cbrt()])
    }

    #[inline]
    pub fn from_oklab(&self, lab: [f32; 3]) -> [f32; 3] {
        let l = apply(&self.lab_to_lms, lab);
        apply(&self.lms_to_work, [l[0] * l[0] * l[0], l[1] * l[1] * l[1], l[2] * l[2] * l[2]])
    }

    /// Converts a display-linear working-space colour to linear sRGB inside
    /// the gamut. Out-of-gamut colours keep their lightness and hue and lose
    /// just enough chroma to fit.
    #[inline]
    pub fn to_srgb_gamut(&self, rgb: [f32; 3]) -> [f32; 3] {
        let srgb = apply(&self.work_to_srgb, rgb);
        let inside = |c: [f32; 3]| c.iter().all(|&v| (-1e-4..=1.0001).contains(&v));
        if inside(srgb) {
            return srgb.map(|v| v.clamp(0.0, 1.0));
        }
        let lab = self.to_oklab(rgb);
        let lightness = lab[0].clamp(0.0, 1.0);
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        let at = |k: f32| apply(&self.work_to_srgb, self.from_oklab([lightness, lab[1] * k, lab[2] * k]));
        for _ in 0..12 {
            let mid = 0.5 * (lo + hi);
            if inside(at(mid)) {
                lo = mid
            } else {
                hi = mid
            }
        }
        at(lo).map(|v| v.clamp(0.0, 1.0))
    }
}

impl Default for Spaces {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
pub fn srgb_encode(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

#[inline]
pub fn srgb_decode(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_round_trips() {
        let cam = camera_matrix("ILCE-7M4").unwrap();
        for (kelvin, tint) in [(2800.0, 0.0), (5500.0, 10.0), (6500.0, -20.0), (9000.0, 35.0)] {
            let wb = wb_from_temperature(&cam, kelvin, tint);
            let (k, t) = temperature_from_wb(&cam, wb);
            assert!((k - kelvin).abs() < 5.0, "{kelvin} K came back as {k}");
            assert!((t - tint).abs() < 0.5, "tint {tint} came back as {t}");
        }
    }

    #[test]
    fn daylight_multipliers_are_plausible() {
        let cam = camera_matrix("ILCE-7M4").unwrap();
        let wb = wb_from_temperature(&cam, 5500.0, 0.0);
        assert!(wb[0] > 1.8 && wb[0] < 3.2 && wb[2] > 1.2 && wb[2] < 2.2, "{wb:?}");
        // Warmer light needs less red gain and more blue gain.
        let tungsten = wb_from_temperature(&cam, 3000.0, 0.0);
        assert!(tungsten[0] < wb[0] && tungsten[2] > wb[2]);
    }

    #[test]
    fn oklab_round_trips_and_neutral_is_achromatic() {
        let s = Spaces::new();
        let c = [0.3, 0.5, 0.1];
        let back = s.from_oklab(s.to_oklab(c));
        for i in 0..3 {
            assert!((back[i] - c[i]).abs() < 1e-4);
        }
        let grey = s.to_oklab([0.18; 3]);
        assert!(grey[1].abs() < 2e-3 && grey[2].abs() < 2e-3, "{grey:?}");
    }

    #[test]
    fn gamut_mapping_stays_in_range_and_keeps_in_gamut_colours() {
        let s = Spaces::new();
        let inside = apply(&s.srgb_to_work, [0.2, 0.4, 0.6]);
        let out = s.to_srgb_gamut(inside);
        assert!((out[0] - 0.2).abs() < 1e-3 && (out[2] - 0.6).abs() < 1e-3);
        let wild = s.to_srgb_gamut([0.0, 0.9, 0.0]);
        assert!(wild.iter().all(|&v| (0.0..=1.0).contains(&v)));
        assert!(wild[1] > wild[0] && wild[1] > wild[2]);
    }
}

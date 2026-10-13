//! Shared native sensor noise policy for default rendering and auto.
use crate::arw::RawImage;
use serde::Serialize;

/// Resolved denoising, separate from the nullable recipe setting.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoiseReduction {
    pub strength: f64,
    pub source: &'static str,
    pub native_sqrt_noise: Option<f32>,
    pub iso_prior: Option<f32>,
}

/// An explicit amount is exact, including zero; otherwise sample the native
/// active mosaic. Framing, resizing and white balance cannot change this policy.
pub fn resolve(raw: &RawImage, explicit: Option<f64>) -> NoiseReduction {
    if let Some(strength) = explicit {
        return NoiseReduction { strength, source: "recipe", native_sqrt_noise: None, iso_prior: None };
    }
    let crop = raw.crop;
    let value = |x: usize, y: usize| {
        let cell = (y & 1) * 2 + (x & 1);
        ((raw.data[y * raw.width + x] as f32 - raw.black[cell]) / (raw.white - raw.black[cell]).max(1.0))
            .clamp(0.0, 1.0)
    };
    // A small stratified sample of native sensor patches, never resized or
    // denoised. Same-colour second differences cancel a smooth gradient.
    // The quietest quarter rejects edges/texture; a noisy flat field still
    // has residuals everywhere. In sqrt encoding the estimate is less
    // sensitive to signal level (shot noise). This is a proxy, not a sensor model.
    let mut residuals = Vec::new();
    let patch_step = crop.width.max(crop.height).div_ceil(64).max(8);
    for y in (crop.y + 6..crop.y + crop.height.saturating_sub(6)).step_by(patch_step) {
        for x in (crop.x + 6..crop.x + crop.width.saturating_sub(6)).step_by(patch_step) {
            let mut energy = 0.0;
            let mut count = 0;
            for dy in 0..4 {
                for dx in 0..4 {
                    let (x, y) = (x + dx, y + dy);
                    let v = value(x, y);
                    if !(0.002..0.8).contains(&v) {
                        continue;
                    }
                    let q = |x, y| value(x, y).sqrt();
                    let r = q(x, y) - (q(x - 2, y) + q(x + 2, y) + q(x, y - 2) + q(x, y + 2)) * 0.25;
                    energy += r * r;
                    count += 1;
                }
            }
            if count >= 8 {
                residuals.push((energy / count as f32 / 1.25).sqrt());
            }
        }
    }
    residuals.sort_by(f32::total_cmp);
    let sigma = (residuals.len() >= 16).then(|| residuals[residuals.len() / 4]);
    let iso = raw.meta.iso.unwrap_or(100).max(50) as f32;
    let prior = (10.0 + 10.0 * (iso / 100.0).log2()).clamp(10.0, 85.0);
    // The quietest patches can miss shadow noise, while uniformly fine
    // texture can imitate grain: retain auto's ISO prior and upper bound.
    let measured = sigma.map(|s| (s / 0.02 * 100.0).clamp(0.0, 100.0));
    let amount = measured.map_or(prior, |m| (0.35 * prior + 0.65 * m).clamp(10.0, (prior + 20.0).min(90.0)));
    NoiseReduction {
        strength: ((amount / 5.0).round() * 5.0) as f64,
        source: if sigma.is_some() { "measured" } else { "iso_prior" },
        native_sqrt_noise: sigma,
        iso_prior: Some(prior),
    }
}

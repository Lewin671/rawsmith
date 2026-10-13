//! Automatic starting point: analyses the scene and proposes tone and colour
//! settings. It is a closed loop: candidate settings are rendered on a small
//! copy of the picture and measured, rather than predicted from the raw data.
//!
//! The result is ordinary recipe values, so it can be inspected and refined.

use crate::arw::RawImage;
use crate::color::{apply, srgb_decode, Spaces};
use crate::error::Result;
use crate::image::Image;
use crate::params::Params;
use crate::pipeline::{self, Size};

/// Long edge of the analysis image.
const ANALYSIS_EDGE: usize = 640;

struct Measure {
    /// Oklab lightness x 100 at percentiles.
    p1: f32,
    p10: f32,
    p50: f32,
    p90: f32,
    p99: f32,
    spread: f32,
    /// Relative chroma (C/L) of the most colourful tenth, not diluted by greys.
    saturation_p90: f32,
    channel_near: f32,
    channel_clipped: f32,
    neutral_bright_share: f32,
    /// Mean chroma x 100 of the whole picture and of its brightest fifth.
    chroma: f32,
    chroma_bright: f32,
    /// Percent of pixels blown to display white.
    clipped: f32,
    /// Mean Oklab a, b x 100 of the near-grey pixels, and their share of the picture.
    cast: [f32; 2],
    neutral_share: f32,
}

fn measure(image: &Image, spaces: &Spaces) -> Measure {
    let mut cast = (0.0f64, 0.0f64, 0usize);
    let mut saturation = Vec::new();
    let mut neutral_bright = 0usize;
    let mut samples: Vec<(f32, f32)> = image
        .data
        .iter()
        .map(|p| {
            let lab = spaces.to_oklab(apply(&spaces.srgb_to_work, p.map(srgb_decode)));
            let sample = (lab[0] * 100.0, (lab[1] * lab[1] + lab[2] * lab[2]).sqrt() * 100.0);
            if sample.0 > 20.0 {
                saturation.push(sample.1 / sample.0);
            }
            if sample.0 > 55.0 && sample.1 < 3.0 {
                neutral_bright += 1;
            }
            if sample.1 < 3.0 && (20.0..90.0).contains(&sample.0) {
                cast = (cast.0 + lab[1] as f64, cast.1 + lab[2] as f64, cast.2 + 1);
            }
            sample
        })
        .collect();
    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    saturation.sort_by(f32::total_cmp);
    let n = samples.len();
    let at = |p: f32| samples[((n - 1) as f32 * p) as usize].0;
    let mean = samples.iter().map(|s| s.0).sum::<f32>() / n as f32;
    let mean_chroma = |s: &[(f32, f32)]| s.iter().map(|s| s.1).sum::<f32>() / s.len().max(1) as f32;
    Measure {
        saturation_p90: saturation.get(saturation.len().saturating_sub(1) * 9 / 10).copied().unwrap_or(0.0),
        channel_near: image.data.iter().filter(|p| p.iter().any(|&v| v >= 0.98)).count() as f32 * 100.0 / n as f32,
        channel_clipped: image.data.iter().filter(|p| p.iter().any(|&v| v >= 0.998)).count() as f32 * 100.0 / n as f32,
        neutral_bright_share: neutral_bright as f32 / n as f32,
        p1: at(0.01),
        p10: at(0.10),
        p50: at(0.50),
        p90: at(0.90),
        p99: at(0.99),
        spread: (samples.iter().map(|s| (s.0 - mean).powi(2)).sum::<f32>() / n as f32).sqrt(),
        chroma: mean_chroma(&samples),
        chroma_bright: mean_chroma(&samples[n * 4 / 5..]),
        clipped: image.data.iter().filter(|p| p.iter().all(|&v| v >= 0.998)).count() as f32 * 100.0 / n as f32,
        cast: [cast.0, cast.1].map(|sum| (sum / cast.2.max(1) as f64 * 100.0) as f32),
        neutral_share: cast.2 as f32 / n as f32,
    }
}

fn smoothstep(lo: f32, hi: f32, v: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn round_to(v: f32, step: f32) -> f64 {
    ((v / step).round() * step * 100.0).round() as f64 / 100.0
}

pub struct Auto {
    pub params: Params,
    /// One line per decision, explaining what was measured and chosen.
    pub notes: Vec<String>,
}

/// Sensor evidence sampled before white balance or highlight reconstruction.
/// Tile flags remain independent of display clipping; partial channel loss
/// is not described as complete loss of highlight texture.
struct Sensor {
    any_pct: f32,
    all_pct: f32,
    lost_bright: f32,
    noise: Option<f32>,
}

fn sensor(raw: &RawImage, params: &Params) -> Sensor {
    let crop = raw.crop;
    let step = crop.width.max(crop.height).div_ceil(ANALYSIS_EDGE * 2).max(1) * 2;
    let (w, h) = (crop.width.div_ceil(step), crop.height.div_ceil(step));
    let value = |x: usize, y: usize| {
        let cell = (y & 1) * 2 + (x & 1);
        ((raw.data[y * raw.width + x] as f32 - raw.black[cell]) / (raw.white - raw.black[cell]).max(1.0))
            .clamp(0.0, 1.0)
    };
    let mut tiles = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (crop.x + x * step, crop.y + y * step);
            let cells = [
                value(sx, sy),
                value((sx + 1).min(crop.x + crop.width - 1), sy),
                value(sx, (sy + 1).min(crop.y + crop.height - 1)),
                value((sx + 1).min(crop.x + crop.width - 1), (sy + 1).min(crop.y + crop.height - 1)),
            ];
            // Same 2% near-saturation margin as mosaic reconstruction.
            let clipped = cells.iter().filter(|&&v| v >= 0.98).count();
            // Green is the exposure reference, before WB makes other channels brighter.
            let green = (0..4).filter(|&i| raw.cfa[i] == 1).map(|i| cells[i]).sum::<f32>() * 0.5;
            tiles.data[y * w + x] = [green, f32::from(clipped > 0), f32::from(clipped == 4)];
        }
    }
    let framed = pipeline::framed(tiles, raw.meta.orientation, params);
    let mut tiles = framed.data;
    tiles.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let n = tiles.len() as f32;
    let bright = &tiles[tiles.len() * 4 / 5..];
    let lost_bright = bright.iter().map(|p| 0.5 * (p[1] + p[2])).sum::<f32>() / bright.len().max(1) as f32;

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
    Sensor {
        any_pct: tiles.iter().map(|p| p[1]).sum::<f32>() / n * 100.0,
        all_pct: tiles.iter().map(|p| p[2]).sum::<f32>() / n * 100.0,
        lost_bright,
        noise: (residuals.len() >= 16).then(|| residuals[residuals.len() / 4]),
    }
}

/// Evidence for an atmospheric veil must vary spatially: subdued, raised
/// dark channels in some blocks and firmer contrast in others. A uniformly
/// grey surface or a smooth blue sky is ambiguous and gets no dehaze.
fn haze_evidence(image: &Image, spaces: &Spaces) -> f32 {
    let mut veiled = 0.0;
    let mut textured = 0.0;
    for by in 0..8 {
        for bx in 0..8 {
            let mut lights = Vec::new();
            let mut floors = Vec::new();
            let mut saturation = 0.0;
            for y in by * image.height / 8..(by + 1) * image.height / 8 {
                for x in bx * image.width / 8..(bx + 1) * image.width / 8 {
                    let p = image.at(x, y);
                    let lab = spaces.to_oklab(apply(&spaces.srgb_to_work, p.map(srgb_decode)));
                    lights.push(lab[0] * 100.0);
                    floors.push(p.into_iter().fold(f32::INFINITY, f32::min));
                    saturation += lab[1].hypot(lab[2]) / lab[0].max(0.2);
                }
            }
            if lights.is_empty() {
                continue;
            }
            lights.sort_by(f32::total_cmp);
            floors.sort_by(f32::total_cmp);
            let n = lights.len();
            let range = lights[(n - 1) * 9 / 10] - lights[(n - 1) / 10];
            let s = saturation / n as f32;
            textured += smoothstep(12.0, 24.0, range);
            let muted = smoothstep(0.025, 0.05, s) * (1.0 - smoothstep(0.10, 0.18, s));
            veiled += muted
                * smoothstep(0.18, 0.35, floors[(n - 1) / 10])
                * smoothstep(2.0, 5.0, range)
                * (1.0 - smoothstep(10.0, 20.0, range))
                * (1.0 - smoothstep(75.0, 85.0, lights[n / 2]));
        }
    }
    smoothstep(0.10, 0.35, veiled / 64.0) * smoothstep(0.05, 0.20, textured / 64.0)
}

/// Proposes settings for `raw`. White balance, geometry and lens settings of
/// `base` are kept; tone and colour settings are replaced.
pub fn auto(raw: &RawImage, base: &Params) -> Result<Auto> {
    let mut params = Params {
        temperature: base.temperature,
        tint: base.tint,
        lens_corrections: base.lens_corrections,
        rotate: base.rotate,
        crop: base.crop,
        sharpen: base.sharpen,
        defringe: base.defringe,
        ..Params::default()
    };
    let sensor = sensor(raw, &params);
    let iso = raw.meta.iso.unwrap_or(100).max(50) as f32;
    let prior = (10.0 + 10.0 * (iso / 100.0).log2()).clamp(10.0, 85.0);
    // Retain a small ISO prior because the quietest patches can miss noise
    // in deep shadows; measured noise separates equally rated exposures.
    let measured = sensor.noise.map(|sigma| (sigma / 0.02 * 100.0).clamp(0.0, 100.0));
    params.denoise =
        round_to(measured.map_or(prior, |m| (0.35 * prior + 0.65 * m).clamp(10.0, (prior + 20.0).min(90.0))), 5.0);
    let scene = pipeline::scene(raw, &params, Size { max_edge: Some(ANALYSIS_EDGE) })?;
    let spaces = Spaces::new();
    let look =
        |p: &Params| measure(&pipeline::finish(&scene.image, &Params { sharpen: 0.0, ..p.clone() }, &spaces), &spaces);
    let default_image = pipeline::finish(&scene.image, &Params { sharpen: 0.0, ..params.clone() }, &spaces);
    let neutral = measure(&default_image, &spaces);
    let mut notes = vec![format!(
        "denoise {:.0}: ISO {iso:.0} prior {prior:.0}; native sensor sqrt-noise {} (quiet-patch proxy, not a calibrated noise model)",
        params.denoise, sensor.noise.map_or_else(|| "unavailable; using ISO prior".into(), |v| format!("{v:.5}"))
    ), format!(
        "measured at default settings: lightness p10 {:.0}, median {:.0}, p90 {:.0}, p99 {:.0}; chroma {:.1} overall, {:.1} in bright areas; saturation C/L p90 {:.3}; channels near clipping {:.2}%",
        neutral.p10, neutral.p50, neutral.p90, neutral.p99, neutral.chroma, neutral.chroma_bright, neutral.saturation_p90, neutral.channel_near
    )];
    // Broad bright neutrals PLUS a meaningful tonal range. Middle-grey
    // flat water/walls cannot establish snow/fog merely by lacking colour.
    // Completely lost whites are not evidence for a diffuse white subject.
    let usable_neutral_share = (neutral.neutral_bright_share - sensor.all_pct / 100.0).max(0.0);
    let high_key = smoothstep(0.35, 0.65, usable_neutral_share)
        * smoothstep(12.0, 24.0, neutral.p90 - neutral.p10)
        * (1.0 - smoothstep(3.0, 6.0, neutral.chroma_bright));
    let colourful = smoothstep(2.5, 6.0, neutral.chroma_bright);
    let backlit = smoothstep(35.0, 60.0, neutral.p90 - neutral.p50);
    notes.push(format!(
        "high-key confidence {high_key:.2}: bright neutral area {:.1}% / usable proxy {:.1}%, p90-p10 range {:.0}; sensor sample any-channel loss {:.2}%, all-channel loss {:.2}%, weighted loss in brightest fifth {:.1}% (partial loss has half weight)",
        neutral.neutral_bright_share * 100.0, usable_neutral_share * 100.0, neutral.p90 - neutral.p10, sensor.any_pct, sensor.all_pct, sensor.lost_bright * 100.0
    ));
    params.shadows = round_to((35.0 - neutral.p10).clamp(0.0, 30.0) * (1.0 - high_key), 5.0);
    params.highlights = round_to(
        -((neutral.p90 - 78.0) * 1.2).clamp(0.0, 25.0) * (1.0 - high_key) * (1.0 - 0.75 * sensor.lost_bright)
            - 10.0 * colourful,
        5.0,
    );
    notes.push(format!(
        "shadows {:+.0}, highlights {:+.0}: p10 {:.0}, p90 {:.0}; reduce highlight suppression where sensor samples have lost information",
        params.shadows, params.highlights, neutral.p10, neutral.p90
    ));

    // A highlight limit cannot demand unlimited foreground darkening.
    // Even with recoverable highlights, retain the metered midtones within
    // four perceptual lightness points. For lost sky in backlight, also
    // impose a quarter-stop bound; leave local separation to the editor.
    let floor_ev = -0.8 + 0.55 * backlit * sensor.lost_bright;
    let floor_light = neutral.p50 - 4.0;
    let mid_limit = neutral.p50 + 2.0 + 18.0 * high_key;
    let bright_limit = 90.0 - 8.0 * colourful;
    let lost = backlit * sensor.lost_bright;
    // A percentile inside completely lost sensor highlights cannot describe
    // recoverable texture. The cutoffs follow the percentile's area, not a
    // scene class: 1% all-channel loss can occupy p99, 10% can occupy p90.
    let p90_limit = if sensor.all_pct >= 10.0 { 100.0 } else { bright_limit + (100.0 - bright_limit) * lost };
    let p99_limit = if sensor.all_pct >= 1.0 { 100.0 } else { 96.0 + 4.0 * lost };
    notes.push(format!(
        "highlight targets L90 <= {p90_limit:.0}, L99 <= {p99_limit:.0}: do not darken percentiles occupied by completely sensor-saturated samples"
    ));
    let limits = |m: &Measure| {
        [
            (m.p50 > mid_limit, "metered midtones"),
            (m.p90 > p90_limit, "recoverable bright areas"),
            (m.p99 > p99_limit, "highlight tonal room"),
            (m.clipped > neutral.clipped + 0.2, "new pure-white clipping"),
            (m.channel_clipped > sensor.any_pct + 0.2, "output channel clipping beyond sampled sensor loss"),
        ]
    };
    let lost_median = sensor.all_pct >= 50.0;
    let max_exposure = if lost_median { 0.0 } else { 0.5 + 0.7 * high_key };
    let (mut lo, mut hi) = (if lost_median { 0.0 } else { floor_ev }, max_exposure);
    let searches = if lost_median { 0 } else { 7 };
    let mut bound = "the adjustment range";
    if lost_median {
        bound = "metering prior; complete sensor loss occupies the median";
        notes.push(
            "exposure held at 0 EV: complete sensor loss occupies the median, so it cannot meter the remaining subject"
                .into(),
        );
    }
    // Find the foreground-preserving floor first (monotonic tone response).
    for _ in 0..searches {
        params.exposure = (lo + hi) as f64 * 0.5;
        let m = look(&params);
        if m.p50 < floor_light {
            lo = params.exposure as f32;
        } else {
            hi = params.exposure as f32;
        }
    }
    let cost_floor = hi.max(floor_ev).min(0.0);
    (lo, hi) = (cost_floor, max_exposure);
    for _ in 0..searches {
        let mid = 0.5 * (lo + hi);
        params.exposure = mid as f64;
        match limits(&look(&params)).iter().find(|(over, _)| *over) {
            Some((_, name)) => {
                hi = mid;
                bound = name;
            }
            None => lo = mid,
        }
    }
    // Round toward the safe side of the foreground floor.
    params.exposure = round_to(lo, 0.05).max((cost_floor as f64 * 20.0).ceil() / 20.0);
    let mut exposed = look(&params);
    let initial_highlights = params.highlights;
    // When global exposure hits its foreground floor, try local highlight
    // compression instead. Accept it only while the metered midtones stay
    // within the same cost bound and a highlight measurement improves.
    while params.highlights > -50.0 && limits(&exposed).iter().skip(1).any(|(over, _)| *over) {
        let trial = Params { highlights: (params.highlights - 5.0).max(-50.0), ..params.clone() };
        let m = look(&trial);
        // A gamut-boundary colour can pin a channel without losing sensor
        // highlight information. Do not flatten it when tone trials cannot
        // reduce that clipping.
        let needs_channel_room = exposed.channel_clipped > sensor.any_pct + 0.2;
        if m.p50 < floor_light
            || (needs_channel_room && m.channel_clipped >= exposed.channel_clipped - 0.05)
            || (m.p90 >= exposed.p90 - 0.1
                && m.p99 >= exposed.p99 - 0.1
                && m.channel_clipped >= exposed.channel_clipped - 0.05)
        {
            break;
        }
        params.highlights = trial.highlights;
        exposed = m;
    }
    notes.push(format!(
        "highlight refinement {initial_highlights:+.0} -> {:+.0}: trial local compression when exposure cannot darken the foreground further; median must remain >= {floor_light:.0}",
        params.highlights
    ));
    if limits(&exposed).iter().any(|(over, _)| *over) {
        bound = "foreground/local-compression bounds; highlight target remains unresolved";
    }
    notes.push(format!(
        "exposure {:+.2} EV, limited by {bound}; foreground bound {cost_floor:+.2} EV / median >= {floor_light:.0}; resulting median {:.0}, p90 {:.0}, p99 {:.0}; saturated highlights cannot regain texture",
        params.exposure, exposed.p50, exposed.p90, exposed.p99
    ));

    let haze = haze_evidence(&default_image, &spaces) * (1.0 - high_key);
    params.dehaze = round_to(15.0 * haze, 5.0);
    notes.push(format!(
        "dehaze {:+.0}: spatial veil confidence {haze:.2}; requires raised dark channels and subdued textured blocks alongside firmer contrast; flat greys and saturated sky alone are insufficient",
        params.dehaze
    ));
    let toned = look(&params);
    // Flat scenes may be deliberately soft; do not force every distribution
    // to a fixed spread or stretch atmospheric blacks to zero.
    params.contrast = round_to(
        10.0 * smoothstep(8.0, 16.0, toned.spread) * (1.0 - smoothstep(18.0, 26.0, toned.spread)) * (1.0 - high_key),
        5.0,
    );
    params.blacks = round_to(-5.0 * smoothstep(8.0, 18.0, toned.p1) * (1.0 - backlit) * (1.0 - high_key), 5.0);
    params.whites = round_to(((94.0 - toned.p99) * 4.0).clamp(0.0, 30.0) * high_key * high_key, 5.0);
    // Verify the proposed white point; it must keep the brightest percent
    // below perceptual 96 and not introduce display clipping.
    while params.whites > 0.0 {
        let m = look(&params);
        if m.p99 <= 96.0 && m.clipped <= neutral.clipped + 0.2 {
            break;
        }
        params.whites -= 5.0;
    }
    notes.push(format!(
        "contrast {:+.0}, blacks {:+.0}, whites {:+.0}: spread {:.0}, p1 {:.0}, p99 {:.0}; retain soft scenes, backlit foreground and highlight room",
        params.contrast, params.blacks, params.whites, toned.spread, toned.p1, toned.p99
    ));

    // Later tone controls must not undo the headroom established above.
    // Back off positive additions, rather than darkening the whole frame.
    let additions = (params.dehaze, params.contrast, params.whites);
    let mut coloured = look(&params);
    while coloured.channel_clipped > exposed.channel_clipped + 0.05 || coloured.clipped > exposed.clipped + 0.2 {
        if params.whites > 0.0 {
            params.whites -= 5.0;
        } else if params.dehaze > 0.0 {
            params.dehaze -= 5.0;
        } else if params.contrast > 0.0 {
            params.contrast -= 5.0;
        } else {
            break;
        }
        coloured = look(&params);
    }
    if additions != (params.dehaze, params.contrast, params.whites) {
        notes.push(format!(
            "tone headroom guard: dehaze {:.0}->{:.0}, contrast {:.0}->{:.0}, whites {:.0}->{:.0}; positive tone additions may not grow channel clipping by more than 0.05 percentage points or pure white by 0.2",
            additions.0, params.dehaze, additions.1, params.contrast, additions.2, params.whites
        ));
    }
    let colour_budget = 20.0
        * smoothstep(1.2, 2.5, coloured.chroma)
        * (1.0 - smoothstep(0.07, 0.16, coloured.saturation_p90))
        * (1.0 - smoothstep(0.1, 1.0, coloured.channel_near));
    params.vibrance = round_to(colour_budget, 5.0);
    while params.vibrance > 0.0 {
        let m = look(&params);
        if m.channel_clipped <= coloured.channel_clipped + 0.05 && m.channel_near <= coloured.channel_near + 0.2 {
            break;
        }
        params.vibrance -= 5.0;
    }
    // Texture, skin and noise cannot be separated reliably by global
    // statistics. Leave clarity for a deliberate visual edit.
    params.clarity = 0.0;
    notes.push(format!(
        "vibrance {:+.0}, clarity 0: saturation C/L p90 {:.3}, channels near clipping {:.2}%, clipped {:.2}%; colour trials limited to +0.05 percentage points clipping / +0.2 near clipping; no reliable evidence for extra local contrast",
        params.vibrance, coloured.saturation_p90, coloured.channel_near, coloured.channel_clipped
    ));
    let [a, b] = toned.cast;
    if high_key > 0.5 && toned.neutral_share > 0.3 && (a.abs() >= 0.4 || b.abs() >= 0.8) {
        notes.push(format!(
            "white balance left as shot; neutral cast a {a:+.1}, b {b:+.1}: inspect whether bright neutrals are tinted or naturally lit before changing temperature/tint"
        ));
    }
    let final_measure = look(&params);
    notes.push(format!(
        "final 640px analysis: median {:.0}, p99 {:.0}, channel clipping {:.2}%, pure white {:.2}%; residual clipping can be sensor loss or an output-gamut boundary; inspect a full-resolution export",
        final_measure.p50, final_measure.p99, final_measure.channel_clipped, final_measure.clipped
    ));
    Ok(Auto { params, notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{arw, synthetic::Synthetic};

    #[test]
    fn sensor_distinguishes_partial_loss_and_respects_framing() {
        let raw = arw::decode(
            &Synthetic::from_camera_rgb(128, 96, |x, _| {
                if x < 32 {
                    [1.0; 3]
                } else if x < 64 {
                    [1.0, 0.1, 0.1]
                } else {
                    [0.1; 3]
                }
            })
            .to_bytes(),
        )
        .unwrap();
        let s = sensor(&raw, &Params::default());
        assert_eq!(s.any_pct, 50.0);
        assert_eq!(s.all_pct, 25.0);
        let mut p = Params::default();
        p.crop.left = 0.5;
        let s = sensor(&raw, &p);
        assert_eq!(s.any_pct, 0.0);
        assert_eq!(s.all_pct, 0.0);
        assert_eq!(s.lost_bright, 0.0);
        p.crop.left = 0.25;
        p.crop.right = 0.5;
        let s = sensor(&raw, &p);
        assert_eq!(s.any_pct, 100.0);
        assert_eq!(s.all_pct, 0.0);
        assert_eq!(s.lost_bright, 0.5);
    }

    #[test]
    fn one_clipped_channel_counts_even_without_display_white() {
        let mut image = Image::new(16, 16);
        image.data.fill([1.0, 0.2, 0.3]);
        let m = measure(&image, &Spaces::new());
        assert_eq!(m.clipped, 0.0);
        assert_eq!(m.channel_clipped, 100.0);
        assert_eq!(m.channel_near, 100.0);
        assert!(m.saturation_p90 > 0.16);
    }
}

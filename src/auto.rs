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
    /// Mean chroma x 100 of the whole picture and of its brightest fifth.
    chroma: f32,
    chroma_bright: f32,
    /// Percent of pixels with a channel at display white.
    clipped: f32,
}

fn measure(image: &Image, spaces: &Spaces) -> Measure {
    let mut samples: Vec<(f32, f32)> = image
        .data
        .iter()
        .map(|p| {
            let lab = spaces.to_oklab(apply(&spaces.srgb_to_work, p.map(srgb_decode)));
            (lab[0] * 100.0, (lab[1] * lab[1] + lab[2] * lab[2]).sqrt() * 100.0)
        })
        .collect();
    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    let n = samples.len();
    let at = |p: f32| samples[((n - 1) as f32 * p) as usize].0;
    let mean = samples.iter().map(|s| s.0).sum::<f32>() / n as f32;
    let mean_chroma = |s: &[(f32, f32)]| s.iter().map(|s| s.1).sum::<f32>() / s.len().max(1) as f32;
    Measure {
        p1: at(0.01),
        p10: at(0.10),
        p50: at(0.50),
        p90: at(0.90),
        p99: at(0.99),
        spread: (samples.iter().map(|s| (s.0 - mean).powi(2)).sum::<f32>() / n as f32).sqrt(),
        chroma: mean_chroma(&samples),
        chroma_bright: mean_chroma(&samples[n * 4 / 5..]),
        clipped: image.data.iter().filter(|p| p.iter().any(|&v| v >= 0.998)).count() as f32 * 100.0 / n as f32,
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

/// Proposes settings for `raw`. White balance, geometry and lens settings of
/// `base` are kept; tone and colour settings are replaced.
pub fn auto(raw: &RawImage, base: &Params) -> Result<Auto> {
    let defaults = Params::default();
    let mut params = Params {
        temperature: base.temperature,
        tint: base.tint,
        lens_corrections: base.lens_corrections,
        rotate: base.rotate,
        crop: base.crop,
        sharpen: base.sharpen,
        denoise: base.denoise,
        ..defaults
    };
    let scene = pipeline::scene(raw, &params, Size { max_edge: Some(ANALYSIS_EDGE) })?;
    let spaces = Spaces::new();
    let look = |p: &Params| measure(&pipeline::finish(&scene.image, &Params { sharpen: 0.0, ..p.clone() }, &spaces), &spaces);
    let mut notes = Vec::new();
    let neutral = look(&params);

    // Exposure: bring the bright part of the picture to a target lightness.
    // Neutral highlights (snow, fog, cloud) should be nearly white; colourful
    // ones (sunset sky) need to stay darker to hold their colour.
    let colourful = smoothstep(5.0, 14.0, neutral.chroma_bright);
    let bright_target = 88.0 - 14.0 * colourful;
    let expose = |params: &mut Params| {
        let (mut lo, mut hi) = (-1.0f32, 2.0f32);
        for _ in 0..7 {
            let mid = 0.5 * (lo + hi);
            params.exposure = mid as f64;
            let m = look(params);
            // Too bright if the highlights pass the target, the midtones wash
            // out, or new clipping appears beyond what the sensor already lost.
            let over = m.p90 > bright_target || m.p50 > 68.0 || m.clipped > scene.sensor_clipped_pct as f32 + 0.5;
            if over { hi = mid } else { lo = mid }
        }
        params.exposure = round_to(lo, 0.05);
    };
    expose(&mut params);
    let exposed = look(&params);
    notes.push(format!(
        "exposure {:+.2} EV: brightest tenth was at lightness {:.0}, target {:.0} ({} highlights)",
        params.exposure,
        neutral.p90,
        bright_target,
        if colourful > 0.5 { "colourful" } else { "neutral" }
    ));

    // Local tone: open deep shadows and hold bright areas when the scene's
    // range is wider than the display's.
    let shadows = ((22.0 - exposed.p10) * 2.5).clamp(0.0, 45.0);
    let highlights = -((exposed.p99 - 90.0) * 5.0).clamp(0.0, 40.0) - 20.0 * colourful;
    if shadows >= 5.0 || highlights <= -5.0 {
        params.shadows = round_to(shadows, 5.0);
        params.highlights = round_to(highlights, 5.0);
        expose(&mut params);
        notes.push(format!(
            "shadows {:+.0}, highlights {:+.0}: darkest tenth at lightness {:.0}, brightest hundredth at {:.0}; exposure re-set to {:+.2}",
            params.shadows, params.highlights, exposed.p10, exposed.p99, params.exposure
        ));
    }

    // Global contrast and black point for flat scenes.
    let toned = look(&params);
    if toned.spread < 17.0 {
        params.contrast = round_to(((17.0 - toned.spread) * 4.0).clamp(0.0, 30.0), 5.0);
    }
    if toned.p1 > 8.0 {
        params.blacks = round_to(-((toned.p1 - 8.0) * 2.5).clamp(0.0, 25.0), 5.0);
    }
    if params.contrast != 0.0 || params.blacks != 0.0 {
        notes.push(format!(
            "contrast {:+.0}, blacks {:+.0}: lightness spread was {:.0} (flat below 17), darkest tones at {:.0}",
            params.contrast, params.blacks, toned.spread, toned.p1
        ));
    }

    // Colour: lift muted scenes more than already vivid ones.
    params.vibrance = round_to(25.0 - 15.0 * smoothstep(6.0, 16.0, toned.chroma), 5.0);
    params.clarity = 10.0;
    notes.push(format!("vibrance {:+.0}, clarity +10: mean colourfulness was {:.0}", params.vibrance, toned.chroma));
    Ok(Auto { params, notes })
}

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
    /// Percent of pixels blown to display white.
    clipped: f32,
    /// Mean Oklab a, b x 100 of the near-grey pixels, and their share of the picture.
    cast: [f32; 2],
    neutral_share: f32,
}

fn measure(image: &Image, spaces: &Spaces) -> Measure {
    let mut cast = (0.0f64, 0.0f64, 0usize);
    let mut samples: Vec<(f32, f32)> = image
        .data
        .iter()
        .map(|p| {
            let lab = spaces.to_oklab(apply(&spaces.srgb_to_work, p.map(srgb_decode)));
            let sample = (lab[0] * 100.0, (lab[1] * lab[1] + lab[2] * lab[2]).sqrt() * 100.0);
            if sample.1 < 3.0 && (20.0..90.0).contains(&sample.0) {
                cast = (cast.0 + lab[1] as f64, cast.1 + lab[2] as f64, cast.2 + 1);
            }
            sample
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

/// Proposes settings for `raw`. White balance, geometry and lens settings of
/// `base` are kept; tone and colour settings are replaced.
pub fn auto(raw: &RawImage, base: &Params) -> Result<Auto> {
    // More noise reduction as the sensor gain rises.
    let iso = raw.meta.iso.unwrap_or(100).max(50) as f32;
    let mut params = Params {
        temperature: base.temperature,
        tint: base.tint,
        lens_corrections: base.lens_corrections,
        rotate: base.rotate,
        crop: base.crop,
        sharpen: base.sharpen,
        denoise: round_to((10.0 + 10.0 * (iso / 100.0).log2()).clamp(10.0, 60.0), 5.0),
        ..Params::default()
    };
    let scene = pipeline::scene(raw, &params, Size { max_edge: Some(ANALYSIS_EDGE) })?;
    let spaces = Spaces::new();
    let look =
        |p: &Params| measure(&pipeline::finish(&scene.image, &Params { sharpen: 0.0, ..p.clone() }, &spaces), &spaces);
    let mut notes = Vec::new();
    let neutral = look(&params);

    if params.denoise > 10.0 {
        notes.push(format!("denoise {:.0}: the photo was taken at ISO {iso:.0}", params.denoise));
    }

    // Scene character. High-key: bright and nearly colourless (snow, fog),
    // meant to stay bright. Colourful highlights (a sunset sky) must stay
    // darker than neutral ones to hold their colour.
    let high_key = (1.0 - smoothstep(1.3, 2.2, neutral.chroma)) * smoothstep(45.0, 56.0, neutral.p50);
    let colourful = smoothstep(2.5, 6.0, neutral.chroma_bright);

    notes.push(format!(
        "measured at default settings: lightness p10 {:.0}, median {:.0}, p90 {:.0}, p99 {:.0}; colourfulness {:.1} overall, {:.1} in bright areas",
        neutral.p10, neutral.p50, neutral.p90, neutral.p99, neutral.chroma, neutral.chroma_bright
    ));

    // Local tone first: a scene wider than the display needs its ends
    // brought in before one exposure can suit both.
    params.shadows = round_to((40.0 - neutral.p10).clamp(0.0, 35.0) * (1.0 - high_key), 5.0);
    params.highlights =
        round_to((-((neutral.p90 - 75.0) * 1.5).clamp(0.0, 30.0) - 10.0 * colourful) * (1.0 - high_key), 5.0);
    if params.shadows != 0.0 || params.highlights != 0.0 {
        notes.push(format!(
            "shadows {:+.0}, highlights {:+.0}: darkest tenth of the picture was at lightness {:.0}, brightest tenth at {:.0}",
            params.shadows, params.highlights, neutral.p10, neutral.p90
        ));
    }

    // Exposure: as bright as possible without passing any limit.
    // Ordinary scenes keep roughly the brightness the photographer metered
    // (dark ones are opened by `shadows` instead); high-key ones go bright.
    let mid_limit = (neutral.p50 + 2.0).max(47.0) * (1.0 - high_key) + 78.0 * high_key;
    let bright_limit = 90.0 - 14.0 * colourful;
    let limits = |m: &Measure| {
        [
            (m.p50 > mid_limit, "midtones"),
            (m.p90 > bright_limit, "bright areas"),
            (m.p99 > 97.0, "the brightest tones"),
            (m.clipped > scene.sensor_clipped_pct as f32 + 0.3, "blown highlights"),
        ]
    };
    // The camera's metering is a strong prior: stay within a stop of it,
    // a little more for high-key scenes that meters underexpose.
    let (mut lo, mut hi) = (-1.0f32, 0.8 + 0.7 * high_key);
    let mut bound = "the adjustment range";
    for _ in 0..7 {
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
    params.exposure = round_to(lo, 0.05);
    notes.push(format!(
        "exposure {:+.2} EV, limited by {bound}: median lightness was {:.0} (limit {:.0}), brightest tenth {:.0} (limit {:.0}); scene read as {}",
        params.exposure,
        neutral.p50,
        mid_limit,
        neutral.p90,
        bright_limit,
        if high_key > 0.5 {
            "high-key (snow or fog)"
        } else if colourful > 0.5 {
            "having colourful highlights"
        } else {
            "ordinary"
        }
    ));

    // Haze lifts the darkest tones well above black and flattens the whole
    // picture; either alone is not evidence enough. High-key scenes are
    // exempt: fog is the subject there.
    let exposed = look(&params);
    let hazy = smoothstep(10.0, 24.0, exposed.p1) * (1.0 - smoothstep(14.0, 18.0, exposed.spread));
    params.dehaze = round_to(25.0 * hazy * (1.0 - high_key), 5.0);
    if params.dehaze != 0.0 {
        notes.push(format!(
            "dehaze {:+.0}: the darkest tones sat at lightness {:.0}, which suggests haze",
            params.dehaze, exposed.p1
        ));
    }

    // Global contrast, black point and, for high-key scenes, white point.
    let toned = look(&params);
    params.contrast = round_to((10.0 + (17.0 - toned.spread) * 3.0).clamp(5.0, 30.0), 5.0);
    if toned.p1 > 8.0 {
        params.blacks = round_to(-((toned.p1 - 8.0) * 2.5).clamp(0.0, 25.0), 5.0);
    }
    params.whites = round_to(((92.0 - toned.p99) * 4.0).clamp(0.0, 40.0) * high_key, 5.0);
    notes.push(format!(
        "contrast {:+.0}, blacks {:+.0}, whites {:+.0}: lightness spread was {:.0}, darkest tones at {:.0}, brightest at {:.0}",
        params.contrast, params.blacks, params.whites, toned.spread, toned.p1, toned.p99
    ));

    // Colour: lift muted scenes more than already vivid ones.
    // Nearly colourless scenes (snow, fog) have nothing for vibrance to act on.
    let coloured = smoothstep(1.2, 2.5, toned.chroma);
    params.vibrance = round_to((30.0 - 15.0 * smoothstep(4.0, 9.0, toned.chroma)) * coloured, 5.0);
    params.clarity = 10.0;
    notes.push(format!("vibrance {:+.0}, clarity +10: mean colourfulness was {:.1}", params.vibrance, toned.chroma));

    // White balance is a matter of mood, so it is reported, never changed.
    let [a, b] = toned.cast;
    // Only where greys dominate a bright scene; elsewhere blue shade and
    // warm light are the scene itself.
    if high_key > 0.5 && toned.neutral_share > 0.3 && (a.abs() >= 0.4 || b.abs() >= 0.8) {
        notes.push(format!(
            "white balance left as shot, but the greys lean {} (neutral cast a {a:+.1}, b {b:+.1}): if snow, fog or cloud looks tinted rather than atmospheric, adjust tint (a) or temperature (b)",
            match (a.abs() >= 0.4, b.abs() >= 0.8) {
                (true, true) => format!("{} and {}", if a > 0.0 { "magenta" } else { "green" }, if b > 0.0 { "yellow" } else { "blue" }),
                (true, false) => (if a > 0.0 { "magenta" } else { "green" }).to_string(),
                _ => (if b > 0.0 { "yellow" } else { "blue" }).to_string(),
            }
        ));
    }
    Ok(Auto { params, notes })
}

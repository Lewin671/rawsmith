//! Automatic finished starting point: analyses the scene and proposes tone and colour
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
use rayon::prelude::*;

/// Long edge of the analysis image.
const ANALYSIS_EDGE: usize = 640;
/// Largest camera-JPEG edge decoded for the tone bound of a cropped recipe.
const CAMERA_EDGE_LIMIT: usize = 8192;

// In sqrt sensor-green light, 0.25 separates a near-white tile from a
// neighbour below about 0.55. This also catches a one-pixel dark line that
// covers only one of the tile's two greens, while rejecting sky gradients.
const FRINGE_EDGE: f32 = 0.25;
// Count each native 2x2 tile once if any of its four neighbours establishes
// risk, then divide by the total native tile count, not occupied intervals.
// Thus > 0.5% still means more than one risky tile per 200 native tiles,
// independent of sampling step (40,369 of 8,073,600 at 6960x4640). It rejects
// isolated hot sites; it is a heuristic, not a calibrated fringe probability.
const FRINGE_EDGE_PCT: f32 = 0.5;

struct Measure {
    /// Oklab lightness x 100 at percentiles.
    p1: f32,
    p10: f32,
    p50: f32,
    p75: f32,
    p90: f32,
    p99: f32,
    spread: f32,
    /// Relative chroma (C/L) of the most colourful tenth, not diluted by greys.
    saturation_p90: f32,
    channel_near: f32,
    channel_clipped: f32,
    /// Gamut-boundary colours below L90 are not highlight overexposure.
    highlight_clipped: f32,
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
    let mut highlight_clipped = 0usize;
    // The colour conversion is the expensive part and independent per pixel;
    // the sums below stay in pixel order so results do not depend on threads.
    let labs: Vec<[f32; 3]> =
        image.data.par_iter().map(|p| spaces.to_oklab(apply(&spaces.srgb_to_work, p.map(srgb_decode)))).collect();
    let mut samples: Vec<(f32, f32)> = image
        .data
        .iter()
        .zip(&labs)
        .map(|(p, lab)| {
            let sample = (lab[0] * 100.0, (lab[1] * lab[1] + lab[2] * lab[2]).sqrt() * 100.0);
            if sample.0 > 90.0 && p.iter().any(|&v| v >= 0.998) {
                highlight_clipped += 1;
            }
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
    drop(labs);
    // Stable, so equal lightness keeps pixel order whatever the thread count.
    samples.par_sort_by(|a, b| a.0.total_cmp(&b.0));
    saturation.par_sort_unstable_by(f32::total_cmp);
    let n = samples.len();
    let at = |p: f32| samples[((n - 1) as f32 * p) as usize].0;
    let mean = samples.iter().map(|s| s.0).sum::<f32>() / n as f32;
    let mean_chroma = |s: &[(f32, f32)]| s.iter().map(|s| s.1).sum::<f32>() / s.len().max(1) as f32;
    Measure {
        saturation_p90: saturation.get(saturation.len().saturating_sub(1) * 9 / 10).copied().unwrap_or(0.0),
        channel_near: image.data.iter().filter(|p| p.iter().any(|&v| v >= 0.98)).count() as f32 * 100.0 / n as f32,
        channel_clipped: image.data.iter().filter(|p| p.iter().any(|&v| v >= 0.998)).count() as f32 * 100.0 / n as f32,
        neutral_bright_share: neutral_bright as f32 / n as f32,
        highlight_clipped: highlight_clipped as f32 * 100.0 / n as f32,
        p1: at(0.01),
        p10: at(0.10),
        p50: at(0.50),
        p75: at(0.75),
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
    fringe_edge_pct: f32,
}

fn sensor(raw: &RawImage, params: &Params) -> Sensor {
    let crop = raw.crop;
    let step = crop.width.max(crop.height).div_ceil(ANALYSIS_EDGE * 2).max(1) * 2;
    let (w, h) = (crop.width.div_ceil(step), crop.height.div_ceil(step));
    let tile = |samples: [u16; 4]| {
        let cells: [f32; 4] = std::array::from_fn(|i| {
            ((samples[i] as f32 - raw.black[i]) / (raw.white - raw.black[i]).max(1.0)).clamp(0.0, 1.0)
        });
        // Same 2% near-saturation margin as mosaic reconstruction.
        let clipped = cells.iter().filter(|&&v| v >= 0.98).count();
        // Green is the exposure reference, before WB makes other channels brighter.
        let green = (0..4).filter(|&i| raw.cfa[i] == 1).map(|i| cells[i]).sum::<f32>() * 0.5;
        [green, f32::from(clipped > 0), f32::from(clipped == 4)]
    };
    let mut tiles = Image::new(w, h);
    let mut edges = Image::new(w, h);
    #[derive(Clone, Copy, Default)]
    struct EdgeTile {
        sqrt_green: f32,
        lost: bool,
        risk: bool,
    }
    let borders_loss =
        |p: EdgeTile, q: EdgeTile| (p.lost || q.lost) && (p.sqrt_green - q.sqrt_green).abs() >= FRINGE_EDGE;
    let interval = step / 2;
    let accumulate = |row: &[EdgeTile], y: usize, edges: &mut Image| {
        for (x, p) in row.iter().enumerate() {
            let out = &mut edges.data[(y / interval) * w + x / interval];
            out[0] += f32::from(p.risk);
            out[1] += 1.0;
        }
    };
    // One pass over the active mosaic, with only two native tile rows and
    // analysis-sized interval counts. Mark both endpoints of each edge,
    // including across intervals; flush a row only after seeing its bottom
    // neighbour. Thin structures cannot fall between stratified samples.
    let mut previous = vec![EdgeTile::default(); crop.width / 2];
    let mut current = previous.clone();
    for y in 0..crop.height / 2 {
        let sy = crop.y + y * 2;
        let start = sy * raw.width + crop.x;
        let top = raw.data[start..start + crop.width].as_chunks::<2>().0;
        let bottom = raw.data[start + raw.width..start + raw.width + crop.width].as_chunks::<2>().0;
        for (x, (top, bottom)) in top.iter().zip(bottom).enumerate() {
            let p = tile([top[0], top[1], bottom[0], bottom[1]]);
            if y % interval == 0 && x % interval == 0 {
                tiles.data[(y / interval) * w + x / interval] = p;
            }
            current[x] = EdgeTile { sqrt_green: p[0].sqrt(), lost: p[1] > 0.0, risk: false };
            if x > 0 && borders_loss(current[x], current[x - 1]) {
                current[x].risk = true;
                current[x - 1].risk = true;
            }
            if y > 0 && borders_loss(current[x], previous[x]) {
                current[x].risk = true;
                previous[x].risk = true;
            }
        }
        if y > 0 {
            accumulate(&previous, y - 1, &mut edges);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    accumulate(&previous, crop.height / 2 - 1, &mut edges);
    let framed = pipeline::framed(tiles, raw.meta.orientation, params);
    let edges = pipeline::framed(edges, raw.meta.orientation, params);
    // Frame counts together before dividing, so partial intervals carry
    // their actual native area and rotation interpolates both consistently.
    let (risky, total) =
        edges.data.iter().fold((0.0f64, 0.0f64), |(risk, count), p| (risk + p[0] as f64, count + p[1] as f64));
    let fringe_edge_pct = (risky / total * 100.0) as f32;
    let mut tiles = framed.data;
    tiles.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let n = tiles.len() as f32;
    let bright = &tiles[tiles.len() * 4 / 5..];
    let lost_bright = bright.iter().map(|p| 0.5 * (p[1] + p[2])).sum::<f32>() / bright.len().max(1) as f32;

    Sensor {
        any_pct: tiles.iter().map(|p| p[1]).sum::<f32>() / n * 100.0,
        all_pct: tiles.iter().map(|p| p[2]).sum::<f32>() / n * 100.0,
        lost_bright,
        fringe_edge_pct,
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
    propose(raw, base, None, "camera tone bound unavailable: original file bytes not supplied".into())
}

/// Uses the embedded JPEG as a tone sanity bound, after the same verified
/// colour conversion as `compare`. Missing, corrupt or unverified camera
/// colour falls back to the finished policy, with the reason in decisions.
pub fn auto_with_file(raw: &RawImage, base: &Params, file: &[u8]) -> Result<Auto> {
    let _stage = crate::profile::Stage::new("auto total");
    let panels = [crate::compare::Panel { kind: "camera", params: Params::default(), recipe_file: None }];
    // Frame first, then reduce: a tight crop of an already reduced JPEG
    // would leave too few pixels and shift the reference percentiles.
    let kept = (base.crop.right - base.crop.left).min(base.crop.bottom - base.crop.top).clamp(0.05, 1.0);
    let camera_edge = ((ANALYSIS_EDGE as f64 / kept).ceil() as usize).min(CAMERA_EDGE_LIMIT);
    match crate::compare::analysis_row(raw, file, &panels, camera_edge) {
        Ok((image, report, true)) => {
            // The single camera panel is already upright. Apply the recipe's
            // rotation/crop once, without running developing controls on it.
            let image = pipeline::framed(image, 1, base);
            let long = image.width.max(image.height);
            let image = if long > ANALYSIS_EDGE {
                let scale = ANALYSIS_EDGE as f64 / long as f64;
                image.resized(
                    ((image.width as f64 * scale).round() as usize).max(1),
                    ((image.height as f64 * scale).round() as usize).max(1),
                )
            } else {
                image
            };
            let camera = measure(&image, &Spaces::new());
            let note = format!(
                "camera tone bound: verified sRGB; conversion {}; matched framing; median {:.1}, upper midtone L75 {:.1}, L75-L50 {:.1}; two-point lightness tolerance, not a look to copy",
                report["panels"][0]["color_conversion"], camera.p50, camera.p75, camera.p75 - camera.p50
            );
            propose(raw, base, Some(camera), note)
        }
        Ok(_) => propose(
            raw,
            base,
            None,
            "camera tone bound unavailable: colour space unverified; no unconverted JPEG measurements used".into(),
        ),
        Err(error) => {
            propose(raw, base, None, format!("camera tone bound unavailable: {error}; using finished fallback"))
        }
    }
}

fn propose(raw: &RawImage, base: &Params, camera: Option<Measure>, camera_note: String) -> Result<Auto> {
    let mut params = Params {
        temperature: base.temperature,
        tint: base.tint,
        lens_corrections: base.lens_corrections,
        rotate: base.rotate,
        crop: base.crop,
        sharpen: base.sharpen,
        defringe: base.defringe,
        denoise: base.denoise,
        ..Params::default()
    };
    let sensor = sensor(raw, &params);
    let defringe_reason = if base.defringe > 0.0 {
        "kept non-zero base recipe strength"
    } else if sensor.fringe_edge_pct > FRINGE_EDGE_PCT {
        // Mid-range suppression, with no global purple desaturation.
        params.defringe = 50.0;
        "enabled moderate suppression for clipped fine-edge risk"
    } else {
        "kept base recipe strength; insufficient clipped-edge coverage"
    };
    let noise = crate::noise::resolve(raw, params.denoise);
    let scene = pipeline::scene(raw, &params, Size { max_edge: Some(ANALYSIS_EDGE) })?;
    let spaces = Spaces::new();
    // The searches below often return to settings they have already tried
    // (a bisection end point, a trial repeated after it is accepted).
    let measured: std::cell::RefCell<Vec<(Params, std::rc::Rc<Measure>)>> = Default::default();
    let look = |p: &Params| {
        if let Some((_, known)) = measured.borrow().iter().find(|(tried, _)| tried == p) {
            return known.clone();
        }
        let fresh = std::rc::Rc::new(measure(
            &pipeline::finish(&scene.image, &Params { sharpen: 0.0, ..p.clone() }, &spaces),
            &spaces,
        ));
        measured.borrow_mut().push((p.clone(), fresh.clone()));
        fresh
    };
    let default_image = pipeline::finish(&scene.image, &Params { sharpen: 0.0, ..params.clone() }, &spaces);
    let neutral = measure(&default_image, &spaces);
    let mut notes = vec![format!(
        "denoise {:.0}: source {}; ISO prior {}; native sensor sqrt-noise {} (quiet-patch proxy, not a calibrated noise model)",
        noise.strength, noise.source, noise.iso_prior.map_or_else(|| "not used".into(), |v| format!("{v:.0}")), noise.native_sqrt_noise.map_or_else(|| if params.denoise.is_some() { "not sampled (explicit recipe strength)".into() } else { "unavailable; using ISO prior".into() }, |v| format!("{v:.5}"))
    ), format!(
        "measured at default settings: lightness p10 {:.0}, median {:.0}, p90 {:.0}, p99 {:.0}; chroma {:.1} overall, {:.1} in bright areas; saturation C/L p90 {:.3}; channels near clipping {:.2}%",
        neutral.p10, neutral.p50, neutral.p90, neutral.p99, neutral.chroma, neutral.chroma_bright, neutral.saturation_p90, neutral.channel_near
    )];
    notes.push(camera_note);
    notes.push(format!(
        "defringe {:.0}: sensor-loss edges {:.2}% of framed native tiles (all tiles aggregated per sampling interval; any-site loss >= 98%, neighbouring sqrt-green difference >= {FRINGE_EDGE:.2}); threshold > {FRINGE_EDGE_PCT:.2}%; {defringe_reason}; risk proxy, not proof of purple fringing",
        params.defringe, sensor.fringe_edge_pct
    ));
    // Broad bright neutrals PLUS a meaningful tonal range. Middle-grey
    // flat water/walls cannot establish snow/fog merely by lacking colour.
    // Completely lost whites are not evidence for a diffuse white subject.
    let usable_neutral_share = (neutral.neutral_bright_share - sensor.all_pct / 100.0).max(0.0);
    let high_key = smoothstep(0.35, 0.65, usable_neutral_share)
        * smoothstep(12.0, 24.0, neutral.p90 - neutral.p10)
        * (1.0 - smoothstep(3.0, 6.0, neutral.chroma_bright));
    let backlit = smoothstep(35.0, 60.0, neutral.p90 - neutral.p50);
    notes.push(format!(
        "high-key confidence {high_key:.2}: bright neutral area {:.1}% / usable proxy {:.1}%, p90-p10 range {:.0}; sensor sample any-channel loss {:.2}%, all-channel loss {:.2}%, weighted loss in brightest fifth {:.1}% (partial loss has half weight)",
        neutral.neutral_bright_share * 100.0, usable_neutral_share * 100.0, neutral.p90 - neutral.p10, sensor.any_pct, sensor.all_pct, sensor.lost_bright * 100.0
    ));
    let shadow_lift = (35.0 - neutral.p10).clamp(0.0, 30.0);
    let subject_lift = backlit * (55.0 - neutral.p50).clamp(0.0, 50.0);
    params.shadows = round_to(shadow_lift.max(subject_lift) * (1.0 - high_key), 5.0);
    params.highlights =
        round_to(-((neutral.p90 - 90.0) * 1.5).clamp(0.0, 20.0) * (1.0 - high_key) * (1.0 - sensor.lost_bright), 5.0);
    notes.push(format!(
        "shadows {:+.0}, highlights {:+.0}: p10 {:.0}, median {:.0}, p90 {:.0}, backlight confidence {backlit:.2}; open shaded subjects, use local compression for recoverable highlights and leave sensor-lost sky alone",
        params.shadows, params.highlights, neutral.p10, neutral.p50, neutral.p90
    ));

    // A small bright region is not evidence that the whole exposure is wrong.
    // Only a broadly bright non-high-key frame can justify negative EV; snow
    // and sensor-saturated sky cannot. The shoulder/local control carries the
    // ordinary highlight cost, rather than four points of foreground light.
    let broad_overexposure = neutral.p50 > 85.0 && high_key < 0.35 && sensor.all_pct < 1.0;
    let floor_ev = if broad_overexposure { -0.3 } else { 0.0 };
    let lost_median = sensor.all_pct >= 50.0;
    // A feasible foreground floor must also fit on the 0.05 EV recipe grid.
    let camera_lift = camera.as_ref().is_some_and(|m| m.p50 > neutral.p50 + 2.0 || m.p75 > neutral.p75 + 2.0);
    let max_exposure = ((if lost_median {
        0.0
    } else if camera_lift {
        1.2
    } else {
        0.5 + 0.7 * high_key
    }) * 20.0)
        .floor()
        / 20.0;
    let default_floor = neutral.p50 - 1.0 + 4.0 * backlit;
    let requested_floor_light = camera.as_ref().map_or(default_floor, |m| default_floor.max(m.p50 - 2.0));
    // L75 checks upper midtones rather than a p90 occupied by sky. If sensor
    // loss itself occupies this percentile, it cannot meter the subject.
    let requested_floor_upper = camera.as_ref().filter(|_| sensor.all_pct < 25.0).map(|m| m.p75 - 2.0);
    let ceiling = look(&Params { exposure: max_exposure as f64, highlights: 0.0, ..params.clone() });
    // An impossible camera floor must not veto every finished-look trial.
    // Keep a point of room below the attainable ceiling, and disclose the
    // unmet original bound instead of claiming that the fallback matched it.
    let floor_light = requested_floor_light.min(ceiling.p50 - 1.0);
    let floor_upper = requested_floor_upper.map(|v| v.min(ceiling.p75 - 1.0));
    if floor_light < requested_floor_light || floor_upper != requested_floor_upper {
        notes.push(format!(
            "requested brightness floor unreachable at {max_exposure:+.2} EV: median {requested_floor_light:.1}, L75 {}; using attainable median {floor_light:.1}, L75 {} to preserve a finished fallback; original camera bound still reported",
            requested_floor_upper.map_or_else(|| "not used".into(), |v| format!("{v:.1}")), floor_upper.map_or_else(|| "not used".into(), |v| format!("{v:.1}"))
        ));
    }
    let foreground_ok = |m: &Measure| m.p50 >= floor_light && floor_upper.is_none_or(|floor| m.p75 >= floor);
    let mid_limit = (neutral.p50 + 2.0 + 18.0 * high_key + 6.0 * backlit).max(floor_light + 2.0);
    let lost = backlit * sensor.lost_bright;
    let p90_limit = if sensor.all_pct >= 10.0 { 100.0 } else { 96.0 + 4.0 * lost };
    let p99_limit = if sensor.all_pct >= 1.0 { 100.0 } else { 99.0 + lost };
    notes.push(format!(
        "highlight targets L90 <= {p90_limit:.0}, L99 <= {p99_limit:.0}: small bright areas use the shoulder/local control; negative EV allowed only for broad non-high-key overexposure ({broad_overexposure}); do not darken completely sensor-saturated percentiles"
    ));
    let limits = |m: &Measure| {
        [
            (m.p50 > mid_limit, "metered midtones"),
            (m.p90 > p90_limit, "recoverable bright areas"),
            (m.p99 > p99_limit, "highlight tonal room"),
            (m.clipped > neutral.clipped + 0.2, "new pure-white clipping"),
            (m.highlight_clipped > sensor.any_pct + 0.2, "bright output channel clipping beyond sampled sensor loss"),
        ]
    };
    let proposed_highlights = params.highlights;
    params.exposure = max_exposure as f64;
    while !foreground_ok(&look(&params)) && params.highlights < 0.0 {
        params.highlights = (params.highlights + 5.0).min(0.0);
    }
    if params.highlights != proposed_highlights {
        notes.push(format!(
            "initial highlight compression {proposed_highlights:+.0} -> {:+.0}: weakened to keep median >= {floor_light:.1} within the exposure ceiling {max_exposure:+.2} EV",
            params.highlights
        ));
    }
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
        if !foreground_ok(&m) {
            lo = params.exposure as f32;
        } else {
            hi = params.exposure as f32;
        }
    }
    // Compression can require a positive floor even for a bright scene.
    // Highlight targets yield to this floor when the two cannot coexist.
    let cost_floor = hi.max(floor_ev);
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
    params.exposure =
        round_to(lo, 0.05).max(((cost_floor as f64 - 1e-6) * 20.0).ceil() / 20.0).min(round_to(max_exposure, 0.05));
    let mut exposed = look(&params);
    let initial_highlights = params.highlights;
    // When global exposure hits its foreground floor, try local highlight
    // compression instead. Accept it only while the metered midtones stay
    // within the same cost bound and a highlight measurement improves.
    let highlight_floor = round_to(-30.0 * (1.0 - sensor.lost_bright), 5.0);
    while params.highlights > highlight_floor && limits(&exposed).iter().skip(1).any(|(over, _)| *over) {
        let trial = Params { highlights: (params.highlights - 5.0).max(highlight_floor), ..params.clone() };
        let m = look(&trial);
        // A gamut-boundary colour can pin a channel without losing sensor
        // highlight information. Do not flatten it when tone trials cannot
        // reduce that clipping.
        let needs_channel_room = exposed.highlight_clipped > sensor.any_pct + 0.2;
        if !foreground_ok(&m)
            || (needs_channel_room && m.highlight_clipped >= exposed.highlight_clipped - 0.05)
            || (m.p90 >= exposed.p90 - 0.1
                && m.p99 >= exposed.p99 - 0.1
                && m.highlight_clipped >= exposed.highlight_clipped - 0.05)
        {
            break;
        }
        params.highlights = trial.highlights;
        exposed = m;
    }
    notes.push(format!(
        "highlight refinement {initial_highlights:+.0} -> {:+.0}: trial local compression when exposure cannot darken the foreground further; median must remain >= {floor_light:.1}; local suppression bounded at {highlight_floor:+.0} by recoverable sensor evidence",
        params.highlights
    ));
    if limits(&exposed).iter().any(|(over, _)| *over) {
        bound = "foreground/local-compression bounds; highlight target remains unresolved";
    }
    notes.push(format!(
        "exposure {:+.2} EV, limited by {bound}; foreground bound {cost_floor:+.2} EV / median >= {floor_light:.1}; resulting median {:.1}, p90 {:.0}, p99 {:.0}; saturated highlights cannot regain texture",
        params.exposure, exposed.p50, exposed.p90, exposed.p99
    ));

    let haze = haze_evidence(&default_image, &spaces) * (1.0 - high_key);
    params.dehaze = round_to(15.0 * haze, 5.0);
    notes.push(format!(
        "dehaze {:+.0}: spatial veil confidence {haze:.2}; requires raised dark channels and subdued textured blocks alongside firmer contrast; flat greys and saturated sky alone are insufficient",
        params.dehaze
    ));
    let toned = look(&params);
    // Ambiguous/flat distributions get a modest finished default. Only
    // established broad contrast or high-key evidence reduces it.
    params.contrast = round_to(
        (15.0 + 5.0 * (1.0 - smoothstep(10.0, 18.0, toned.spread)))
            * (1.0 - smoothstep(22.0, 32.0, toned.spread))
            * (1.0 - 0.5 * high_key),
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
    // Add controls separately so a failed trial backs off its responsible
    // control, including negative black-point shifts. Neither clipping nor
    // the foreground floor may be undone by later tone adjustments.
    let additions = (params.dehaze, params.contrast, params.blacks, params.whites);
    (params.dehaze, params.contrast, params.blacks, params.whites) = (0.0, 0.0, 0.0, 0.0);
    // Contrast can need a little exposure back. Trial the pair rather than
    // rejecting all punch just because the curve lowered a dark median.
    let compensate = |trial: &mut Params| {
        if foreground_ok(&look(trial)) || lost_median {
            return;
        }
        let (mut lo, mut hi) = (trial.exposure as f32, max_exposure);
        trial.exposure = hi as f64;
        if !foreground_ok(&look(trial)) {
            return;
        }
        for _ in 0..7 {
            let mid = (lo + hi) * 0.5;
            trial.exposure = mid as f64;
            if foreground_ok(&look(trial)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        trial.exposure = (((hi as f64 - 1e-6) * 20.0).ceil() / 20.0).min(round_to(max_exposure, 0.05));
    };
    let safe_tone = |m: &Measure| {
        foreground_ok(m) && m.highlight_clipped <= exposed.highlight_clipped + 0.2 && m.clipped <= exposed.clipped + 0.2
    };
    for (control, mut value) in
        [("dehaze", additions.0), ("contrast", additions.1), ("blacks", additions.2), ("whites", additions.3)]
    {
        while value != 0.0 {
            let mut trial = params.clone();
            match control {
                "dehaze" => trial.dehaze = value,
                "contrast" => trial.contrast = value,
                "blacks" => trial.blacks = value,
                "whites" => trial.whites = value,
                _ => unreachable!(),
            }
            compensate(&mut trial);
            let m = look(&trial);
            if safe_tone(&m) && (control != "whites" || m.p99 <= 96.0) {
                params = trial;
                break;
            }
            value -= value.signum() * 5.0;
        }
    }
    if let Some(camera) = camera.as_ref().filter(|_| sensor.all_pct < 25.0) {
        let gap_floor = (camera.p75 - camera.p50 - 2.0).max(0.0);
        let mut current = look(&params);
        let before = params.contrast;
        let mut contrast_bound = "+30 contrast ceiling";
        while current.p75 - current.p50 < gap_floor && params.contrast < 30.0 {
            let mut trial = Params { contrast: params.contrast + 5.0, ..params.clone() };
            compensate(&mut trial);
            let m = look(&trial);
            if !safe_tone(&m) {
                contrast_bound = "foreground/bright-clipping guard";
                break;
            }
            if m.p75 - m.p50 <= current.p75 - current.p50 + 0.1 {
                contrast_bound = "global contrast cannot improve upper-midtone separation at this brightness";
                break;
            }
            params = trial;
            current = m;
        }
        notes.push(format!(
            "camera upper-midtone contrast bound: L75-L50 >= {gap_floor:.1}; contrast {before:.0}->{:.0}, resulting gap {:.1}; {}",
            params.contrast, current.p75 - current.p50,
            if current.p75 - current.p50 >= gap_floor { "met".into() } else { format!("unresolved: {contrast_bound}") }
        ));
    }
    let coloured = look(&params);
    if additions != (params.dehaze, params.contrast, params.blacks, params.whites) {
        notes.push(format!(
            "tone headroom/foreground guard: dehaze {:.0}->{:.0}, contrast {:.0}->{:.0}, blacks {:.0}->{:.0}, whites {:.0}->{:.0}; each control must keep median >= {floor_light:.1}, bright-channel clipping growth <= 0.2 percentage points and pure-white growth <= 0.2; resulting median {:.1}",
            additions.0, params.dehaze, additions.1, params.contrast, additions.2, params.blacks, additions.3, params.whites, coloured.p50
        ));
    }
    notes.push(format!(
        "contrast {:+.0}, blacks {:+.0}, whites {:+.0}: spread {:.0}, p1 {:.0}, p99 {:.0}; finished contrast fallback, backlit foreground and highlight room",
        params.contrast, params.blacks, params.whites, toned.spread, toned.p1, toned.p99
    ));
    let colour_budget =
        20.0 * smoothstep(0.5, 1.5, coloured.chroma) * (1.0 - smoothstep(0.14, 0.30, coloured.saturation_p90));
    params.vibrance = round_to(colour_budget, 5.0);
    while params.vibrance > 0.0 {
        let m = look(&params);
        if foreground_ok(&m) && m.highlight_clipped <= coloured.highlight_clipped + 0.2 {
            break;
        }
        params.vibrance -= 5.0;
    }
    // Texture, skin and noise cannot be separated reliably by global
    // statistics. Leave clarity for a deliberate visual edit.
    params.clarity = 0.0;
    notes.push(format!(
        "vibrance {:+.0}, clarity 0: saturation C/L p90 {:.3}, channels near clipping {:.2}%, clipped {:.2}%; colour trials keep median >= {floor_light:.1} and are limited to +0.2 percentage points bright-channel clipping (L > 90); gamut-boundary colours below L90 and small near-clipped areas do not veto colour elsewhere; no reliable evidence for extra local contrast",
        params.vibrance, coloured.saturation_p90, coloured.channel_near, coloured.channel_clipped
    ));
    let [a, b] = toned.cast;
    if high_key > 0.5 && toned.neutral_share > 0.3 && (a.abs() >= 0.4 || b.abs() >= 0.8) {
        notes.push(format!(
            "white balance left as shot; neutral cast a {a:+.1}, b {b:+.1}: inspect whether bright neutrals are tinted or naturally lit before changing temperature/tint"
        ));
    }
    let final_measure = look(&params);
    if let Some(camera) = &camera {
        notes.push(format!(
            "camera sanity bound {}: median floor {:.1} (default {:.1}), L75 floor {}; final median {:.1}, L75 {:.1}; {}",
            if camera.p50 - 2.0 > default_floor || camera_lift { "raised brightness bounds" } else { "checked" },
            requested_floor_light, default_floor, requested_floor_upper.map_or_else(|| "skipped: sensor loss occupies upper midtones".into(), |v| format!("{v:.1}")), final_measure.p50, final_measure.p75,
            if final_measure.p50 >= requested_floor_light && requested_floor_upper.is_none_or(|v| final_measure.p75 >= v) { "brightness floors met" } else { "bound remains unresolved at bounded exposure/local controls; sensor loss or scene range prevents matching camera brightness" }
        ));
    }
    notes.push(format!(
        "final 640px analysis: exposure {:+.2} EV, median {:.0}, p99 {:.0}, channel clipping {:.2}%, pure white {:.2}%; residual clipping can be sensor loss or an output-gamut boundary; inspect a full-resolution export",
        params.exposure, final_measure.p50, final_measure.p99, final_measure.channel_clipped, final_measure.clipped
    ));
    Ok(Auto { params, notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{arw, synthetic::Synthetic};

    fn grey(level: f32) -> [f32; 3] {
        [level / (2600.0 / 1024.0), level, level / (1600.0 / 1024.0)]
    }

    fn native_flat() -> RawImage {
        // Real A7 IV active dimensions, with CFA-aligned padding so evidence
        // must respect the sensor crop rather than the stored mosaic size.
        let mut raw = arw::decode(&Synthetic::new(16, 16, vec![512; 16 * 16]).to_bytes()).unwrap();
        raw.width = 7008;
        raw.height = 4672;
        raw.crop = arw::Crop { x: 24, y: 16, width: 6960, height: 4640 };
        raw.data = vec![512; raw.width * raw.height];
        reset_native_lines(&mut raw, &[], false);
        raw
    }

    fn reset_native_lines(raw: &mut RawImage, lines: &[usize], horizontal: bool) {
        let crop = raw.crop;
        for (y, row) in raw.data.chunks_mut(raw.width).skip(crop.y).take(crop.height).enumerate() {
            let row = &mut row[crop.x..crop.x + crop.width];
            row.fill(raw.white as u16);
            if horizontal {
                if lines.contains(&y) {
                    row.fill(512);
                }
            } else {
                for &x in lines {
                    row[x] = 512;
                }
            }
        }
    }

    fn analysis(raw: &RawImage, params: &Params) -> (Measure, Measure, Measure) {
        let scene = pipeline::scene(raw, params, Size { max_edge: Some(ANALYSIS_EDGE) }).unwrap();
        let spaces = Spaces::new();
        let look = |p: &Params| {
            measure(&pipeline::finish(&scene.image, &Params { sharpen: 0.0, ..p.clone() }, &spaces), &spaces)
        };
        (
            look(&Params::default()),
            look(&Params { dehaze: 0.0, contrast: 0.0, blacks: 0.0, whites: 0.0, vibrance: 0.0, ..params.clone() }),
            look(params),
        )
    }

    #[test]
    fn recoverable_bright_neutrals_keep_positive_foreground_floor() {
        let raw =
            arw::decode(&Synthetic::from_camera_rgb(128, 96, |_, y| grey(if y < 24 { 0.9 } else { 0.35 })).to_bytes())
                .unwrap();
        assert_eq!(sensor(&raw, &Params::default()).any_pct, 0.0);
        let result = auto(&raw, &Params::default()).unwrap();
        let (neutral, exposed, finished) = analysis(&raw, &result.params);
        let floor = neutral.p50 - 4.0;
        assert!(exposed.p50 >= floor, "initial compression discarded the floor: {} < {floor}", exposed.p50);
        assert!(finished.p50 >= floor);
        assert!(result.params.exposure > 0.0, "highlight compression needs a positive exposure floor");
        assert!(result.notes.iter().any(|n| n.contains("target remains unresolved")));
        result.params.validate().unwrap();
    }

    #[test]
    fn later_tone_trials_keep_foreground_floor() {
        let raw =
            arw::decode(&Synthetic::from_camera_rgb(128, 96, |_, y| grey(if y < 24 { 0.9 } else { 0.055 })).to_bytes())
                .unwrap();
        let result = auto(&raw, &Params::default()).unwrap();
        let (neutral, exposed, finished) = analysis(&raw, &result.params);
        let floor = neutral.p50 - 4.0;
        assert!(exposed.p50 >= floor, "exposure stage must already satisfy the floor");
        assert!(finished.p50 >= floor, "later controls buried the foreground: {} < {floor}", finished.p50);
        // The accepted slider amount may change with the base curve; the
        // foreground lightness floor above is the invariant.
        result.params.validate().unwrap();
    }

    #[test]
    fn clipped_sky_with_thin_dark_lines_enables_defringe() {
        // One-pixel branches across a sensor-lost sky, with a dark foreground.
        for sky in [[1.0; 3], [1.0, 0.8, 0.5]] {
            let raw = arw::decode(
                &Synthetic::from_camera_rgb(128, 96, |x, y| if y >= 40 || x % 16 < 1 { grey(0.02) } else { sky })
                    .to_bytes(),
            )
            .unwrap();
            let result = auto(&raw, &Params::default()).unwrap();
            assert_eq!(result.params.defringe, 50.0);
            assert!(result.notes.iter().any(|n| n.contains("defringe 50") && n.contains("sensor-loss edges")));
            result.params.validate().unwrap();
            for threads in [1, 3] {
                let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
                let repeated = pool.install(|| auto(&raw, &Params::default()).unwrap());
                assert_eq!(repeated.params, result.params);
                assert_eq!(repeated.notes, result.notes);
            }
            for defringe in [5.0, 65.0, 100.0] {
                let base = Params { defringe, ..Params::default() };
                assert_eq!(auto(&raw, &base).unwrap().params.defringe, defringe);
            }
            let mut foreground = Params::default();
            foreground.crop.top = 0.5;
            assert_eq!(auto(&raw, &foreground).unwrap().params.defringe, 0.0);
        }
    }

    #[test]
    fn native_periodic_edge_coverage_is_independent_of_sampling_phase() {
        let mut raw = native_flat();
        let mut interior = Params::default();
        // Keep complete periods and real neighbours at both frame edges:
        // otherwise a line on a sensor boundary has only one neighbour.
        interior.crop.left = 48.0 / raw.crop.width as f64;
        interior.crop.right = 1.0 - interior.crop.left;
        for phase in [0, 1, 5, 6, 11, 12, 23, 46, 47] {
            let lines: Vec<_> = (phase..raw.crop.width).step_by(48).collect();
            reset_native_lines(&mut raw, &lines, false);
            // A dark column makes one darker tile and two risky neighbours:
            // three of each 24 native tiles = 12.5%, not 0% or 25%.
            assert_eq!(sensor(&raw, &interior).fringe_edge_pct, 12.5, "phase {phase}");
            let risky_columns = lines.len() * 3 - usize::from(!(2..46).contains(&phase));
            let expected = (risky_columns as f64 / (raw.crop.width / 2) as f64 * 100.0) as f32;
            assert_eq!(sensor(&raw, &Params::default()).fringe_edge_pct, expected, "full crop phase {phase}");
        }
    }

    #[test]
    fn native_thin_lines_cross_the_coverage_gate_at_every_phase() {
        let mut raw = native_flat();
        // Five separated full-height lines cover 15/3480 native tile
        // columns (0.431%); six cover 18/3480 (0.517%), bracketing 0.5%.
        for phase in [0, 1, 6, 11] {
            let lines: Vec<_> = (0..6).map(|i| 48 + i * 960 + phase).collect();
            reset_native_lines(&mut raw, &lines[..5], false);
            let below = sensor(&raw, &Params::default()).fringe_edge_pct;
            assert_eq!(below, (15.0f64 / 3480.0 * 100.0) as f32, "five lines, phase {phase}");
            assert!(below < FRINGE_EDGE_PCT);
            reset_native_lines(&mut raw, &lines, false);
            let above = sensor(&raw, &Params::default()).fringe_edge_pct;
            assert_eq!(above, (18.0f64 / 3480.0 * 100.0) as f32, "six lines, phase {phase}");
            assert!(above > FRINGE_EDGE_PCT);
            let result = auto(&raw, &Params::default()).unwrap();
            assert_eq!(result.params.defringe, 50.0, "six lines, phase {phase}");
            assert!(result.notes.iter().any(|n| n.contains("sensor-loss edges 0.52%")
                && n.contains("all tiles aggregated")
                && n.contains("threshold > 0.50%")));
        }
        // Exercise vertical adjacency and the last partial sampling row
        // (4640 % 12 == 8). Its denominator must be actual native tiles.
        for phase in [0, 1, 6, 11] {
            let lines: Vec<_> = (0..6).map(|i| 48 + i * 720 + phase).collect();
            reset_native_lines(&mut raw, &lines, true);
            let evidence = sensor(&raw, &Params::default());
            assert_eq!(evidence.fringe_edge_pct, (18.0f64 / 2320.0 * 100.0) as f32, "horizontal phase {phase}");
            assert!(evidence.fringe_edge_pct > FRINGE_EDGE_PCT);
        }
    }

    #[test]
    fn flat_sensor_loss_without_edges_does_not_enable_defringe() {
        let raw = arw::decode(&Synthetic::from_camera_rgb(128, 96, |_, _| [1.0; 3]).to_bytes()).unwrap();
        assert_eq!(sensor(&raw, &Params::default()).fringe_edge_pct, 0.0);
        assert_eq!(auto(&raw, &Params::default()).unwrap().params.defringe, 0.0);
        let isolated = arw::decode(
            &Synthetic::from_camera_rgb(128, 96, |x, y| {
                if (64..66).contains(&x) && (48..50).contains(&y) {
                    [1.0; 3]
                } else {
                    grey(0.02)
                }
            })
            .to_bytes(),
        )
        .unwrap();
        let evidence = sensor(&isolated, &Params::default());
        assert!(evidence.fringe_edge_pct > 0.0 && evidence.fringe_edge_pct < FRINGE_EDGE_PCT);
        assert_eq!(auto(&isolated, &Params::default()).unwrap().params.defringe, 0.0);
    }

    #[test]
    fn purple_objects_without_sensor_loss_leave_defringe_unchanged() {
        let raw = arw::decode(
            &Synthetic::from_camera_rgb(128, 96, |x, _| if x % 16 < 4 { [0.3, 0.015, 0.24] } else { grey(0.2) })
                .to_bytes(),
        )
        .unwrap();
        assert_eq!(sensor(&raw, &Params::default()).any_pct, 0.0);
        assert_eq!(auto(&raw, &Params::default()).unwrap().params.defringe, 0.0);
        for defringe in [5.0, 65.0, 100.0] {
            let base = Params { defringe, ..Params::default() };
            assert_eq!(auto(&raw, &base).unwrap().params.defringe, defringe);
        }
    }

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
        assert_eq!(m.highlight_clipped, 0.0, "a moderate-lightness gamut boundary is not highlight overexposure");
        assert!(m.saturation_p90 > 0.16);
    }
}

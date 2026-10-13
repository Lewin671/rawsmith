//! Numeric descriptions of a picture, so an assistant can judge a rendering
//! from numbers as well as from the image itself.

use rayon::prelude::*;
use serde::Serialize;

use crate::adjust::band_weights;
use crate::arw::RawImage;
use crate::color::{apply, srgb_decode, Spaces};
use crate::image::Image;
use crate::params::HSL_BANDS;

#[derive(Debug, Clone, Serialize)]
pub struct Tones {
    /// Perceptual lightness (0 black .. 100 white) at these percentiles of
    /// the picture's pixels.
    pub p1: f64,
    pub p5: f64,
    pub p25: f64,
    pub p50: f64,
    pub p75: f64,
    pub p95: f64,
    pub p99: f64,
    pub mean: f64,
    /// Standard deviation of lightness: a measure of global contrast.
    pub spread: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageStats {
    pub width: usize,
    pub height: usize,
    pub tones: Tones,
    /// Percent of pixels blown to pure white (every channel at maximum).
    pub clipped_highlights_pct: f64,
    /// Percent of pixels with at least one channel at maximum: includes vivid
    /// colours at the edge of what the output can show, such as a sunset sky.
    pub channel_clipped_pct: f64,
    /// Percent at maximum for R, G and B separately (same rendered pixel set).
    pub channel_clipped_pct_rgb: [f64; 3],
    /// Percent of pixels at pure black.
    pub crushed_shadows_pct: f64,
    /// Share of pixels (percent) in 10 equal lightness bins from black to white.
    pub histogram: Vec<f64>,
    /// Mean lightness of a 3x3 grid of zones, rows from top to bottom.
    pub zones: [[f64; 3]; 3],
    /// Mean and 95th-percentile colourfulness (Oklab chroma x 100; ~4 is
    /// nearly neutral, ~10 moderately colourful, >20 vivid).
    pub chroma_mean: f64,
    pub chroma_p95: f64,
    /// Average colour offset of nearly neutral pixels (Oklab a, b x 100);
    /// a clear offset suggests a colour cast. a: green(-) to magenta(+),
    /// b: blue(-) to yellow(+).
    pub neutral_cast: [f64; 2],
    /// Percent of colourful pixels falling in each HSL band.
    pub hues_pct: std::collections::BTreeMap<String, f64>,
}

fn round(v: f32) -> f64 {
    (v as f64 * 10.0).round() / 10.0
}

fn round2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

/// Statistics of an sRGB-encoded 0..1 image.
pub fn image_stats(image: &Image, spaces: &Spaces) -> ImageStats {
    // Sample at most ~400k pixels on a regular grid: plenty for percentiles.
    let step = ((image.width * image.height) as f32 / 400_000.0).sqrt().ceil().max(1.0) as usize;
    let mut lightness = Vec::new();
    let mut chroma = Vec::new();
    // Clipping needs every pixel: a regular tone-sampling grid can miss thin
    // saturated lights or CFA-related patterns entirely. Integer sums are
    // independent of Rayon scheduling.
    let counts = image
        .data
        .par_iter()
        .map(|p| {
            let channels = p.map(|v| (v >= 0.998) as usize);
            [
                channels.iter().all(|&n| n != 0) as usize,
                channels.iter().any(|&n| n != 0) as usize,
                p.iter().all(|&v| v <= 0.008) as usize,
                channels[0],
                channels[1],
                channels[2],
            ]
        })
        .reduce(|| [0; 6], |a, b| std::array::from_fn(|c| a[c] + b[c]));
    let pixel_count = image.data.len().max(1) as f32;
    let mut zones = [[(0.0f64, 0usize); 3]; 3];
    let mut cast = (0.0f64, 0.0f64, 0usize);
    let mut hues = [0.0f64; 8];
    for y in (0..image.height).step_by(step) {
        for x in (0..image.width).step_by(step) {
            let p = image.at(x, y);
            let lab = spaces.to_oklab(apply(&spaces.srgb_to_work, p.map(srgb_decode)));
            let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            let l = lab[0] * 100.0;
            lightness.push(l);
            chroma.push(c * 100.0);
            let zone = &mut zones[(y * 3 / image.height).min(2)][(x * 3 / image.width).min(2)];
            zone.0 += l as f64;
            zone.1 += 1;
            if c < 0.03 && (20.0..90.0).contains(&l) {
                cast = (cast.0 + lab[1] as f64, cast.1 + lab[2] as f64, cast.2 + 1);
            }
            if c > 0.04 {
                let (i, j, t) = band_weights(lab[2].atan2(lab[1]).to_degrees());
                hues[i] += (1.0 - t) as f64;
                hues[j] += t as f64;
            }
        }
    }
    let n = lightness.len().max(1);
    let mean = lightness.iter().sum::<f32>() / n as f32;
    let spread = (lightness.iter().map(|l| (l - mean) * (l - mean)).sum::<f32>() / n as f32).sqrt();
    let mut histogram = vec![0.0f32; 10];
    for &l in &lightness {
        histogram[((l / 10.0) as usize).min(9)] += 100.0 / n as f32;
    }
    lightness.sort_by(|a, b| a.total_cmp(b));
    chroma.sort_by(|a, b| a.total_cmp(b));
    let pct = |sorted: &[f32], p: f32| sorted[((sorted.len() - 1) as f32 * p / 100.0) as usize];
    let hue_total: f64 = hues.iter().sum::<f64>().max(1.0);
    ImageStats {
        width: image.width,
        height: image.height,
        tones: Tones {
            p1: round(pct(&lightness, 1.0)),
            p5: round(pct(&lightness, 5.0)),
            p25: round(pct(&lightness, 25.0)),
            p50: round(pct(&lightness, 50.0)),
            p75: round(pct(&lightness, 75.0)),
            p95: round(pct(&lightness, 95.0)),
            p99: round(pct(&lightness, 99.0)),
            mean: round(mean),
            spread: round(spread),
        },
        clipped_highlights_pct: round2(counts[0] as f32 * 100.0 / pixel_count),
        channel_clipped_pct: round2(counts[1] as f32 * 100.0 / pixel_count),
        channel_clipped_pct_rgb: std::array::from_fn(|c| round2(counts[c + 3] as f32 * 100.0 / pixel_count)),
        crushed_shadows_pct: round2(counts[2] as f32 * 100.0 / pixel_count),
        histogram: histogram.into_iter().map(round).collect(),
        zones: zones.map(|row| row.map(|(sum, count)| round((sum / count.max(1) as f64) as f32))),
        chroma_mean: round(chroma.iter().sum::<f32>() / n as f32),
        chroma_p95: round(pct(&chroma, 95.0)),
        neutral_cast: if cast.2 > n / 200 {
            [round((cast.0 / cast.2 as f64) as f32 * 100.0), round((cast.1 / cast.2 as f64) as f32 * 100.0)]
        } else {
            [0.0, 0.0]
        },
        hues_pct: HSL_BANDS
            .iter()
            .zip(hues)
            .map(|(name, v)| (name.to_string(), round((v / hue_total * 100.0) as f32)))
            .collect(),
    }
}

/// How one colour family is rendered: its share of the colourful pixels and
/// their mean chroma and lightness (Oklab x 100).
#[derive(Debug, Clone, Serialize)]
pub struct BandLook {
    pub share_pct: f64,
    pub chroma: f64,
    pub lightness: f64,
}

/// A compact description of a rendering for comparing two versions of the
/// same framing: the tonal anchors, clipping and the colours that carry it.
#[derive(Debug, Clone, Serialize)]
pub struct Look {
    pub p1: f64,
    pub p5: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub spread: f64,
    pub chroma_mean: f64,
    pub clipped_highlights_pct: f64,
    pub crushed_shadows_pct: f64,
    /// Colour families holding at least 5% of the colourful pixels.
    pub colours: std::collections::BTreeMap<String, BandLook>,
}

pub fn look(image: &Image, spaces: &Spaces) -> Look {
    let stats = image_stats(image, spaces);
    let step = ((image.width * image.height) as f32 / 400_000.0).sqrt().ceil().max(1.0) as usize;
    // Weight, chroma and lightness sums per band, on the same grid as the tones.
    let mut bands = [[0.0f64; 3]; 8];
    for y in (0..image.height).step_by(step) {
        for x in (0..image.width).step_by(step) {
            let lab = spaces.to_oklab(apply(&spaces.srgb_to_work, image.at(x, y).map(srgb_decode)));
            let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            if c > 0.04 {
                let (i, j, t) = band_weights(lab[2].atan2(lab[1]).to_degrees());
                for (band, weight) in [(i, 1.0 - t), (j, t)] {
                    let weight = weight as f64;
                    bands[band][0] += weight;
                    bands[band][1] += weight * c as f64 * 100.0;
                    bands[band][2] += weight * lab[0] as f64 * 100.0;
                }
            }
        }
    }
    let total: f64 = bands.iter().map(|b| b[0]).sum::<f64>().max(1.0);
    let tenth = |v: f64| (v * 10.0).round() / 10.0;
    Look {
        p1: stats.tones.p1,
        p5: stats.tones.p5,
        p50: stats.tones.p50,
        p95: stats.tones.p95,
        p99: stats.tones.p99,
        spread: stats.tones.spread,
        chroma_mean: stats.chroma_mean,
        clipped_highlights_pct: stats.clipped_highlights_pct,
        crushed_shadows_pct: stats.crushed_shadows_pct,
        colours: HSL_BANDS
            .iter()
            .zip(bands)
            .filter(|(_, [weight, ..])| *weight / total >= 0.05)
            .map(|(name, [weight, chroma, lightness])| {
                (
                    name.to_string(),
                    BandLook {
                        share_pct: tenth(weight / total * 100.0),
                        chroma: tenth(chroma / weight),
                        lightness: tenth(lightness / weight),
                    },
                )
            })
            .collect(),
    }
}

/// Where an edit gave something up against a reference rendering of the same
/// framing. These are the losses an editor tends not to see while pursuing
/// an idea: a lifted black point, flattened contrast, paled or thinned colour
/// and new pure white. Each is an observation to check by eye, not a fault:
/// fog is meant to be soft and a silhouette dark.
pub fn losses(reference: &Look, edit: &Look) -> Vec<String> {
    let mut notes = Vec::new();
    if edit.p1 >= reference.p1 + 5.0 && edit.p1 >= 8.0 {
        notes.push(format!(
            "black point lifted: darkest tones at {:.0} against {:.0}; without a true dark the picture reads flat or hazy (blacks, contrast or a curve restore it)",
            edit.p1, reference.p1
        ));
    }
    if edit.spread < reference.spread * 0.8 && reference.spread >= 8.0 {
        notes.push(format!(
            "flatter: lightness spread {:.0} against {:.0}; lifted shadows and pulled highlights have closed the tonal range",
            edit.spread, reference.spread
        ));
    }
    if edit.clipped_highlights_pct >= reference.clipped_highlights_pct + 1.0 {
        notes.push(format!(
            "more pure white: {:.1}% against {:.1}%; a brightened sky or highlight has lost its tone",
            edit.clipped_highlights_pct, reference.clipped_highlights_pct
        ));
    }
    if edit.crushed_shadows_pct >= reference.crushed_shadows_pct + 2.0 {
        notes.push(format!(
            "more pure black: {:.1}% against {:.1}%",
            edit.crushed_shadows_pct, reference.crushed_shadows_pct
        ));
    }
    for (name, was) in &reference.colours {
        if was.share_pct < 8.0 || was.chroma < 6.0 {
            continue;
        }
        let Some(now) = edit.colours.get(name) else {
            notes.push(format!(
                "{name} has nearly gone: it held {:.0}% of the colour at chroma {:.0}",
                was.share_pct, was.chroma
            ));
            continue;
        };
        let paler = now.lightness >= was.lightness + 6.0;
        if now.chroma < was.chroma * 0.8 {
            notes.push(format!(
                "{name} is weaker{}: chroma {:.0} against {:.0}{}",
                if paler { " and paler" } else { "" },
                now.chroma,
                was.chroma,
                if paler { format!(", lightness {:.0} against {:.0}; a sky or water of this colour looks washed out", now.lightness, was.lightness) } else { String::new() }
            ));
        } else if now.chroma > was.chroma * 1.5 && now.chroma >= 20.0 {
            notes.push(format!(
                "{name} is much stronger: chroma {:.0} against {:.0}; check that it is still believable",
                now.chroma, was.chroma
            ));
        }
    }
    notes
}

#[derive(Debug, Clone, Serialize)]
pub struct RawStats {
    /// Percent of each colour's photosites that saturated on the sensor
    /// (red, green, blue). Saturated data cannot be recovered by editing.
    pub saturated_pct: [f64; 3],
    /// Percent of 2x2 CFA cells with a saturated photosite of each colour.
    /// Green counts a cell if either green site saturated.
    pub saturated_cells_pct: [f64; 3],
    /// Percent of 2x2 CFA cells with any saturated photosite. Full active
    /// area, before hot-pixel repair, white balance, geometry or resizing.
    pub sensor_clipped_pct: f64,
    /// Stops between the 99.9th-percentile green sample and sensor
    /// saturation: how much highlight headroom the exposure left.
    pub headroom_stops: f64,
    /// Median green level in stops below saturation: how dark the capture is.
    pub median_stops_below_saturation: f64,
}

/// Exact sensor clipping counts. All complete CFA cells in the active area,
/// with the same black-subtracted 98% threshold used by reconstruction.
/// Photosite and cell percentages have different green denominators.
pub fn sensor_clipping(raw: &RawImage) -> ([f64; 3], [f64; 3], f64) {
    let crop = raw.crop;
    let (w, h) = (crop.width / 2, crop.height / 2);
    let totals = (0..h)
        .into_par_iter()
        .map(|y| {
            let mut sums = [0usize; 7];
            for x in 0..w {
                let mut mask = [false; 3];
                for cell in 0..4 {
                    let (sx, sy) = (crop.x + x * 2 + cell % 2, crop.y + y * 2 + cell / 2);
                    let tile = (sy & 1) * 2 + (sx & 1);
                    let c = raw.cfa[tile] as usize;
                    let v = (raw.data[sy * raw.width + sx] as f32 - raw.black[tile])
                        / (raw.white - raw.black[tile]).max(1.0);
                    let clipped = v >= crate::demosaic::CLIP_MARGIN;
                    sums[c] += clipped as usize;
                    mask[c] |= clipped;
                }
                for c in 0..3 {
                    sums[c + 3] += mask[c] as usize;
                }
                sums[6] += mask.into_iter().any(|v| v) as usize;
            }
            sums
        })
        .reduce(|| [0; 7], |a, b| std::array::from_fn(|c| a[c] + b[c]));
    let count = w * h;
    let pct = |n: usize, d: usize| round2(n as f32 * 100.0 / d.max(1) as f32);
    (
        std::array::from_fn(|c| pct(totals[c], count * if c == 1 { 2 } else { 1 })),
        std::array::from_fn(|c| pct(totals[c + 3], count)),
        pct(totals[6], count),
    )
}

pub fn raw_stats(raw: &RawImage) -> RawStats {
    let crop = raw.crop;
    let (saturated_pct, saturated_cells_pct, sensor_clipped_pct) = sensor_clipping(raw);
    let mut greens = Vec::new();
    // Every 4th CFA tile in each direction is a dense enough sample.
    for y in (crop.y..crop.y + crop.height).step_by(8) {
        for x in (crop.x..crop.x + crop.width).step_by(8) {
            for cell in 0..4 {
                let (sx, sy) = (x + cell % 2, y + cell / 2);
                let tile = (sy & 1) * 2 + (sx & 1);
                let v =
                    (raw.data[sy * raw.width + sx] as f32 - raw.black[tile]) / (raw.white - raw.black[tile]).max(1.0);
                let c = raw.cfa[tile] as usize;
                if c == 1 {
                    greens.push(v.max(1e-5));
                }
            }
        }
    }
    greens.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| greens[((greens.len() - 1) as f64 * p) as usize];
    RawStats {
        saturated_pct,
        saturated_cells_pct,
        sensor_clipped_pct,
        headroom_stops: round2((-at(0.999).log2()).max(0.0)),
        median_stops_below_saturation: round2(-at(0.5).log2()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{arw, synthetic::Synthetic};

    #[test]
    fn clipping_counts_include_pixels_between_tone_samples() {
        let mut image = Image::new(1000, 1000);
        image.data.fill([0.5; 3]);
        for y in (1..1000).step_by(2) {
            for x in (1..1000).step_by(2) {
                image.data[y * 1000 + x] = [1.0, 0.5, 0.5];
            }
        }
        let got = image_stats(&image, &Spaces::new());
        assert_eq!(got.clipped_highlights_pct, 0.0);
        assert_eq!(got.channel_clipped_pct, 25.0);
        assert_eq!(got.channel_clipped_pct_rgb, [25.0, 0.0, 0.0]);
    }

    #[test]
    fn sensor_sites_and_cells_have_explicit_green_denominators() {
        // Only one of the two green sites in each cell is saturated.
        let raw = arw::decode(
            &Synthetic::from_camera_rgb(32, 32, |x, y| if x % 2 == 1 && y % 2 == 0 { [1.0; 3] } else { [0.1; 3] })
                .to_bytes(),
        )
        .unwrap();
        let got = raw_stats(&raw);
        assert_eq!(got.saturated_pct, [0.0, 50.0, 0.0]);
        assert_eq!(got.saturated_cells_pct, [0.0, 100.0, 0.0]);
        assert_eq!(got.sensor_clipped_pct, 100.0);
        for edge in [None, Some(8)] {
            let scene = crate::pipeline::scene(
                &raw,
                &crate::params::Params::default(),
                crate::pipeline::Size { max_edge: edge },
            )
            .unwrap();
            assert_eq!(scene.sensor_clipped_pct, got.sensor_clipped_pct);
        }
    }
}

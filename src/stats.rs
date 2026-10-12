//! Numeric descriptions of a picture, so an assistant can judge a rendering
//! from numbers as well as from the image itself.

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
    let (mut clipped, mut channel_clipped, mut crushed) = (0usize, 0usize, 0usize);
    let mut zones = [[(0.0f64, 0usize); 3]; 3];
    let mut cast = (0.0f64, 0.0f64, 0usize);
    let mut hues = [0.0f64; 8];
    for y in (0..image.height).step_by(step) {
        for x in (0..image.width).step_by(step) {
            let p = image.at(x, y);
            clipped += p.iter().all(|&v| v >= 0.998) as usize;
            channel_clipped += p.iter().any(|&v| v >= 0.998) as usize;
            crushed += p.iter().all(|&v| v <= 0.008) as usize;
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
        clipped_highlights_pct: round2(clipped as f32 * 100.0 / n as f32),
        channel_clipped_pct: round2(channel_clipped as f32 * 100.0 / n as f32),
        crushed_shadows_pct: round2(crushed as f32 * 100.0 / n as f32),
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

#[derive(Debug, Clone, Serialize)]
pub struct RawStats {
    /// Percent of each colour's photosites that saturated on the sensor
    /// (red, green, blue). Saturated data cannot be recovered by editing.
    pub saturated_pct: [f64; 3],
    /// Stops between the 99.9th-percentile green sample and sensor
    /// saturation: how much highlight headroom the exposure left.
    pub headroom_stops: f64,
    /// Median green level in stops below saturation: how dark the capture is.
    pub median_stops_below_saturation: f64,
}

pub fn raw_stats(raw: &RawImage) -> RawStats {
    let crop = raw.crop;
    let mut saturated = [0usize; 3];
    let mut count = [0usize; 3];
    let mut greens = Vec::new();
    // Every 4th CFA tile in each direction is a dense enough sample.
    for y in (crop.y..crop.y + crop.height).step_by(8) {
        for x in (crop.x..crop.x + crop.width).step_by(8) {
            for cell in 0..4 {
                let (sx, sy) = (x + cell % 2, y + cell / 2);
                let tile = (sy & 1) * 2 + (sx & 1);
                let v = (raw.data[sy * raw.width + sx] as f32 - raw.black[tile]) / (raw.white - raw.black[tile]);
                let c = raw.cfa[tile] as usize;
                count[c] += 1;
                saturated[c] += (v >= 0.98) as usize;
                if c == 1 {
                    greens.push(v.max(1e-5));
                }
            }
        }
    }
    greens.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| greens[((greens.len() - 1) as f64 * p) as usize];
    let pct = |c: usize| round2(saturated[c] as f32 * 100.0 / count[c].max(1) as f32);
    RawStats {
        saturated_pct: [pct(0), pct(1), pct(2)],
        headroom_stops: round2((-at(0.999).log2()).max(0.0)),
        median_stops_below_saturation: round2(-at(0.5).log2()),
    }
}

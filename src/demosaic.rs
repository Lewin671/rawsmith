//! Mosaic preparation, demosaicing and highlight reconstruction.

use rayon::prelude::*;

use crate::arw::RawImage;
use crate::image::Image;

/// Black-subtracted, white-balanced mosaic of the picture area, scaled so the
/// sensor's saturation point is `clip[c]` for colour `c` (1.0 before white
/// balance).
pub struct Mosaic {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub cfa: [u8; 4],
    /// Value at which each colour channel saturates.
    pub clip: [f32; 3],
}

impl Mosaic {
    #[inline]
    fn color(&self, x: usize, y: usize) -> usize {
        self.cfa[(y & 1) * 2 + (x & 1)] as usize
    }
}

impl Mosaic {
    /// Fraction of prepared 2x2 cells with a photosite at its limit, after
    /// stuck-site repair and WB but before interpolation. Public reports use
    /// `stats::sensor_clipping` on the original active-area samples instead.
    pub fn saturated_fraction(&self) -> f32 {
        let (w, h) = (self.width / 2, self.height / 2);
        let saturated: usize = (0..h)
            .into_par_iter()
            .map(|y| {
                (0..w)
                    .filter(|&x| {
                        (0..4).any(|cell| {
                            self.data[(y * 2 + cell / 2) * self.width + x * 2 + cell % 2]
                                >= self.clip[self.cfa[cell] as usize] * CLIP_MARGIN
                        })
                    })
                    .count()
            })
            .sum();
        saturated as f32 / (w * h).max(1) as f32
    }
}

pub fn prepare(raw: &RawImage, wb: [f32; 3]) -> Mosaic {
    let crop = raw.crop;
    let mut data = vec![0.0f32; crop.width * crop.height];
    let scale: Vec<f32> = (0..4).map(|i| wb[raw.cfa[i] as usize] / (raw.white - raw.black[i]).max(1.0)).collect();
    // A stuck photosite stands far outside all four of its same-colour
    // neighbours; real detail, blurred by the lens, never does.
    let spike = 0.004 * (raw.white - raw.black[0]).max(1.0);
    data.par_chunks_mut(crop.width).enumerate().for_each(|(y, row)| {
        let sy = y + crop.y;
        let at = |x: usize, y: usize| raw.data[y * raw.width + x] as f32;
        for (x, out) in row.iter_mut().enumerate() {
            let sx = x + crop.x;
            let cell = (sy & 1) * 2 + (sx & 1);
            let mut v = at(sx, sy);
            if sx >= 2 && sy >= 2 && sx + 2 < raw.width && sy + 2 < raw.height {
                let around = [at(sx - 2, sy), at(sx + 2, sy), at(sx, sy - 2), at(sx, sy + 2)];
                let (lo, hi) = around.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &n| (lo.min(n), hi.max(n)));
                let (black, range) = (raw.black[cell], (hi - lo).max(spike));
                if v - black > 2.0 * (hi - black) + spike && v > hi + 4.0 * range {
                    v = hi;
                } else if v - black < 0.5 * (lo - black) - spike && v < lo - 4.0 * range {
                    v = lo;
                }
            }
            *out = (v - raw.black[cell]) * scale[cell];
        }
    });
    // The crop origin is even, so the CFA tile is unchanged.
    Mosaic { width: crop.width, height: crop.height, data, cfa: raw.cfa, clip: wb }
}

/// Half-resolution demosaic: each 2x2 CFA tile becomes one pixel, with
/// neutral CFA phase suppression; used only for automatic candidate analysis.
pub fn half(m: &Mosaic) -> Image {
    let (w, h) = (m.width / 2, m.height / 2);
    let mut out = Image::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let mut sum = [0.0f32; 3];
            let mut saturated = [false; 3];
            let (mut green_lo, mut green_hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for cell in 0..4 {
                let c = m.cfa[cell] as usize;
                let v = m.data[(y * 2 + cell / 2) * m.width + x * 2 + cell % 2];
                sum[c] += v;
                if c == 1 {
                    green_lo = green_lo.min(v);
                    green_hi = green_hi.max(v);
                }
                saturated[c] |= v >= m.clip[c] * CLIP_MARGIN;
            }
            // A green that averages one clipped and one unclipped sample must
            // still read as clipped for highlight reconstruction.
            let g = if saturated[1] { m.clip[1] } else { sum[1] * 0.5 };
            *p = [sum[0], g, sum[2]];
            let lo = p.iter().copied().fold(f32::INFINITY, f32::min);
            let hi = p.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            if !saturated.iter().any(|&v| v) && hi - lo > 0.02 && green_hi - green_lo > 0.1 * green_hi {
                if let Some(smooth) = supported_chromaticity(m, x * 2, y * 2) {
                    // A coloured line's faint outer pixels can be locally
                    // near-neutral. Colour support one CFA tile away protects
                    // that entire detail rather than desaturating its wings.
                    if !neutral_neighbourhood(m, x * 2, y * 2, smooth) {
                        continue;
                    }
                    // Neutral CFA phase differences are luminance, not colour.
                    // Average all four photosites to retain their exposure.
                    let level = (sum[0] + sum[1] + sum[2]) * 0.25;
                    let mean = (smooth[0] + 2.0 * smooth[1] + smooth[2]) * 0.25;
                    *p = smooth.map(|v| v * level / mean.max(1e-8));
                }
            }
        }
    });
    out
}

/// Mirror an index into `0..n`, preserving its parity (and so its CFA colour).
#[inline]
fn mirror(i: isize, n: usize) -> usize {
    if i >= 0 && (i as usize) < n {
        return i as usize;
    }
    if n <= 1 {
        return 0;
    }
    let period = 2 * (n as isize - 1);
    let i = i.rem_euclid(period);
    if i < n as isize {
        i as usize
    } else {
        (period - i) as usize
    }
}

/// Binomial low-pass samples each CFA plane with equal total weight. Its
/// 7x7 footprint reduces Bayer phase bias on neutral fine structure without using
/// interpolated RGB as evidence that a genuinely coloured line is neutral.
fn colour_support(m: &Mosaic, x: usize, y: usize) -> [f32; 3] {
    const WEIGHTS: [f32; 7] = [1.0, 6.0, 15.0, 20.0, 15.0, 6.0, 1.0];
    let mut sum = [0.0f32; 3];
    let mut weight = [0.0f32; 3];
    for dy in -3isize..=3 {
        for dx in -3isize..=3 {
            let (nx, ny) = (mirror(x as isize + dx, m.width), mirror(y as isize + dy, m.height));
            let colour = m.color(nx, ny);
            let k = WEIGHTS[(dx + 3) as usize] * WEIGHTS[(dy + 3) as usize];
            sum[colour] += m.data[ny * m.width + nx] * k;
            weight[colour] += k;
        }
    }
    std::array::from_fn::<_, 3, _>(|c| sum[c] / weight[c])
}

// Also permit a moderate, spatially constant cast: neutral dark structure
// against coloured illumination should inherit that supported chromaticity,
// not acquire a different hue from interpolation. A genuine coloured line
// changes the CFA ratios nearby and is excluded.
fn supported_chromaticity(m: &Mosaic, x: usize, y: usize) -> Option<[f32; 3]> {
    let smooth = colour_support(m, x, y);
    let scale = smooth.iter().copied().fold(0.0f32, f32::max).max(1e-8);
    let chroma = (smooth[0] - smooth[1]).abs().max((smooth[2] - smooth[1]).abs()) / scale;
    if is_neutral(smooth) {
        return Some(smooth);
    }
    if chroma > 0.25 || smooth[1] <= 1e-8 {
        return None;
    }
    let ratio = [smooth[0] / smooth[1], smooth[2] / smooth[1]];
    let supported =
        [(-2isize, -2isize), (0, -2), (2, -2), (-2, 0), (2, 0), (-2, 2), (0, 2), (2, 2)].into_iter().all(|(dx, dy)| {
            let q = colour_support(m, mirror(x as isize + dx, m.width), mirror(y as isize + dy, m.height));
            q[1] > 1e-8 && (q[0] / q[1] - ratio[0]).abs().max((q[2] / q[1] - ratio[1]).abs()) < 0.025
        });
    supported.then_some(smooth)
}

fn is_neutral(smooth: [f32; 3]) -> bool {
    let scale = smooth.iter().copied().fold(0.0f32, f32::max).max(1e-8);
    (smooth[0] - smooth[1]).abs().max((smooth[2] - smooth[1]).abs()) < 0.02 * scale
}

fn neutral_neighbourhood(m: &Mosaic, x: usize, y: usize, smooth: [f32; 3]) -> bool {
    // Stable casts already require support in every neighbouring tile. Near
    // neutral detail needs the extra check so coloured wings stay coloured.
    if !is_neutral(smooth) {
        return true;
    }
    [(-2isize, -2isize), (0, -2), (2, -2), (-2, 0), (2, 0), (-2, 2), (0, 2), (2, 2)].into_iter().all(|(dx, dy)| {
        is_neutral(colour_support(m, mirror(x as isize + dx, m.width), mirror(y as isize + dy, m.height)))
    })
}

/// Full-resolution demosaic: edge-directed green interpolation with Laplacian
/// correction (Hamilton-Adams, soft-weighted), then red and blue from
/// interpolated colour differences. Neutral CFA support constrains green at
/// thin high-contrast structures before those differences propagate.
pub fn full(m: &Mosaic) -> Image {
    let (w, h) = (m.width, m.height);
    let at = |x: isize, y: isize| m.data[mirror(y, h) * w + mirror(x, w)];

    let mut green = vec![0.0f32; w * h];
    green.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, g) in row.iter_mut().enumerate() {
            if m.color(x, y) == 1 {
                *g = m.data[y * w + x];
                continue;
            }
            let (xi, yi) = (x as isize, y as isize);
            let c = at(xi, yi);
            let (l, r, u, d) = (at(xi - 1, yi), at(xi + 1, yi), at(xi, yi - 1), at(xi, yi + 1));
            let lap_h = 2.0 * c - at(xi - 2, yi) - at(xi + 2, yi);
            let lap_v = 2.0 * c - at(xi, yi - 2) - at(xi, yi + 2);
            let grad_h = (l - r).abs() + lap_h.abs();
            let grad_v = (u - d).abs() + lap_v.abs();
            let est_h = (l + r) * 0.5 + lap_h * 0.25;
            let est_v = (u + d) * 0.5 + lap_v * 0.25;
            // Interpolate along the direction of least change; the squared
            // weights make the choice decisive on edges and even in flat areas.
            let (wh, wv) = (grad_v * grad_v + 1e-10, grad_h * grad_h + 1e-10);
            let est = (est_h * wh + est_v * wv) / (wh + wv);
            // The Laplacian term can overshoot next to strong edges.
            let (lo, hi) = (l.min(r).min(u).min(d), l.max(r).max(u).max(d));
            *g = est.clamp(lo, hi);
            // A neutral thin structure can make Hamilton-Adams misread
            // luminance curvature as colour. Infer its green from the measured
            // photosite only with neutral or spatially constant CFA colour support.
            // Colour ratio changes protect genuinely coloured thin structures.
            if (c - *g).abs() > 0.02
                && hi - lo > 0.25 * hi
                && c < m.clip[m.color(x, y)] * CLIP_MARGIN
                && *g < m.clip[1] * CLIP_MARGIN
            {
                if let Some(smooth) = supported_chromaticity(m, x, y) {
                    let constrained = c * smooth[1] / smooth[m.color(x, y)].max(1e-8);
                    // Keep measured/clipped green available to reconstruction.
                    if constrained < m.clip[1] * CLIP_MARGIN {
                        *g = constrained;
                    }
                }
            }
        }
    });

    // Colour difference (own colour minus green) at red and blue sites.
    let diff = |x: isize, y: isize| {
        let (x, y) = (mirror(x, w), mirror(y, h));
        m.data[y * w + x] - green[y * w + x]
    };
    let mut out = Image::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let g = green[y * w + x];
            let (xi, yi) = (x as isize, y as isize);
            let own = m.color(x, y);
            let mut rgb = [g; 3];
            if own == 1 {
                // Red and blue neighbours lie along one axis each.
                let horizontal = m.color(x ^ 1, y);
                let vertical = 2 - horizontal;
                rgb[horizontal] = g + (diff(xi - 1, yi) + diff(xi + 1, yi)) * 0.5;
                rgb[vertical] = g + (diff(xi, yi - 1) + diff(xi, yi + 1)) * 0.5;
            } else {
                rgb[own] = m.data[y * w + x];
                let diagonal =
                    diff(xi - 1, yi - 1) + diff(xi + 1, yi - 1) + diff(xi - 1, yi + 1) + diff(xi + 1, yi + 1);
                rgb[2 - own] = g + diagonal * 0.25;
            }
            *p = rgb;
        }
    });
    out
}

/// Samples within this fraction of the saturation point count as clipped.
pub(crate) const CLIP_MARGIN: f32 = 0.98;

// Reconstruction gains confidence over the final tenth of sensor range.
// Start before the saturation threshold so crossing it cannot change hue
// discontinuously; the same interval fades colour as the last channel goes.
const CLIP_KNEE: f32 = 0.9;

fn clip_weight(v: f32) -> f32 {
    let t = ((v - CLIP_KNEE) / (CLIP_MARGIN - CLIP_KNEE)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rebuilds colour channels that saturated on the sensor.
///
/// A clipped channel only says "at least this much". Its true value is
/// estimated from the channels that did not clip and the typical ratio
/// between them, measured on pixels just outside the clipped areas. Without
/// this, blown highlights turn magenta once white balance scales the channels
/// apart. Returns the fraction of pixels that had a channel at its limit
/// after demosaicing (not the full-active-area sensor statistic).
pub fn reconstruct_highlights(image: &mut Image, clip: [f32; 3]) -> f32 {
    let level = clip.map(|v| v * CLIP_KNEE);
    let (w, h) = (image.width, image.height);
    let clipped: Vec<u8> =
        image.data.par_iter().map(|p| (0..3).fold(0u8, |mask, c| mask | (((p[c] >= level[c]) as u8) << c))).collect();
    let count = image.data.par_iter().filter(|p| (0..3).any(|c| p[c] >= clip[c] * CLIP_MARGIN)).count();
    if clipped.par_iter().all(|&mask| mask == 0) {
        return 0.0;
    }

    // Ratios preserve chromaticity as illumination increases, unlike the
    // old additive cube-root offsets. These bordering pixels supply a prior,
    // not evidence that every clipped patch has the same illuminant.
    let reach = (w.max(h) / 400).max(2) as isize;
    let rows: Vec<[f64; 5]> = (0..h)
        .into_par_iter()
        .map(|y| {
            let mut sum = [0.0f64; 5];
            for x in 0..w {
                let p = image.at(x, y);
                if clipped[y * w + x] != 0 || p.iter().any(|&v| v < 0.2 * clip[1]) {
                    continue;
                }
                let near = [(-reach, 0), (reach, 0), (0, -reach), (0, reach)].into_iter().any(|(dx, dy)| {
                    let (nx, ny) = (x as isize + dx, y as isize + dy);
                    nx >= 0
                        && ny >= 0
                        && (nx as usize) < w
                        && (ny as usize) < h
                        && clipped[ny as usize * w + nx as usize] != 0
                });
                if near {
                    let (r, b) = ((p[0] / p[1]) as f64, (p[2] / p[1]) as f64);
                    sum[0] += r;
                    sum[1] += b;
                    sum[2] += r * r;
                    sum[3] += b * b;
                    sum[4] += 1.0;
                }
            }
            sum
        })
        .collect();
    // Fixed row order keeps the colour prior independent of Rayon splits.
    let sum = rows.into_iter().fold([0.0f64; 5], |a, b| std::array::from_fn(|c| a[c] + b[c]));
    let (colour, uncertainty) = if sum[4] > 0.0 {
        let (r, b) = (sum[0] / sum[4], sum[1] / sum[4]);
        let spread = ((sum[2] / sum[4] - r * r).max(0.0).sqrt() / r).max((sum[3] / sum[4] - b * b).max(0.0).sqrt() / b);
        ([r as f32, 1.0, b as f32], spread as f32)
    } else {
        ([1.0; 3], 1.0)
    };
    // A neutral endpoint no darker than any plausible prior-based recovery
    // at the last channel's limit. The clipped core must not form a dark disk
    // surrounded by a brighter reconstructed ring.
    let last_clip = (0..3).map(|c| clip[c] / colour[c]).fold(0.0f32, f32::max);
    let neutral_endpoint = colour.iter().copied().fold(0.0f32, f32::max) * last_clip;

    image.data.par_iter_mut().zip(&clipped).for_each(|(p, &mask)| {
        if mask == 0 {
            return;
        }
        let original = *p;
        let weights = [0, 1, 2].map(|c| clip_weight(original[c] / clip[c]));
        let trust = weights.map(|v| 1.0 - v);
        // A varied border does not establish this patch's missing colour.
        // A single consistent illuminant remains reconstructable (including
        // strongly coloured light); mixed references fade towards neutral.
        let most = weights.into_iter().fold(0.0f32, f32::max);
        let complete = weights.into_iter().fold(1.0f32, f32::min);
        let second = (weights.iter().sum::<f32>() - most - complete).clamp(0.0, 1.0);
        let mut mismatch = 0.0f32;
        for a in 0..3 {
            for b in a + 1..3 {
                let (sa, sb) = (original[a].max(0.0) / colour[a], original[b].max(0.0) / colour[b]);
                mismatch = mismatch.max((sa - sb).abs() / sa.max(sb).max(1e-6) * trust[a] * trust[b]);
            }
        }
        let mut estimates = original;
        let mut neutral_estimates = original;
        for c in 0..3 {
            if weights[c] > 0.0 {
                let (mut sum, mut neutral_sum, mut n) = (0.0, 0.0, 0.0);
                for o in [(c + 1) % 3, (c + 2) % 3] {
                    sum += original[o].max(0.0) * (colour[c] / colour[o]) * trust[o];
                    neutral_sum += original[o].max(0.0) * trust[o];
                    n += trust[o];
                }
                if n > 0.0 {
                    let estimate = sum / n;
                    // Independently keeping a clipped lower bound when the
                    // prior contradicts it would invent a hue.
                    mismatch = mismatch.max((original[c] - estimate).max(0.0) / clip[c] * weights[c]);
                    estimates[c] = estimate.max(original[c]);
                    neutral_estimates[c] = (neutral_sum / n).max(original[c]);
                }
            }
        }
        let t = (mismatch.max(uncertainty) / (1.0 - CLIP_MARGIN)).clamp(0.0, 1.0);
        let untrusted = t * t * (3.0 - 2.0 * t);
        // With two survivors, retain their measured colour and estimate only
        // the missing channel. Desaturating the whole pixel here destroys
        // genuine warm clouds and produces flat white islands.
        for c in 0..3 {
            let estimate = estimates[c] + (neutral_estimates[c] - estimates[c]) * untrusted;
            p[c] += (estimate - p[c]) * weights[c];
        }
        // Once all channels approach their sensor limits, colour is no longer
        // evidence. Fade to neutral continuously instead of switching masks
        // from a coloured reconstruction to white in one pixel.
        // Fade the whole colour only as a second channel loses authority;
        // apply the activation weight after bounding prior confidence, so a
        // varied reference cannot make the fade saturate prematurely.
        let rejected = untrusted * second;
        let neutral_weight = complete.max(rejected);
        let observed = original.iter().copied().fold(0.0f32, f32::max);
        let rebuilt = p.iter().copied().fold(0.0f32, f32::max);
        let neutral_base = observed + (rebuilt - observed).max(0.0) * (1.0 - rejected);
        let neutral = neutral_base + (neutral_endpoint.max(neutral_base) - neutral_base) * complete;
        *p = p.map(|v| v + (neutral - v) * neutral_weight);
    });
    count as f32 / (w * h) as f32
}

/// Distance from a reconstructed pixel within which a pixel can hold light
/// borrowed from it.
pub const BORROWED_SPILL: usize = 5;
/// Radius searched for the covering object and the unclipped setting.
pub const BORROWED_REACH: usize = 7;

/// Neutralises colour in light that partially covered pixels borrow from a
/// neighbouring clipped region.
///
/// A twig's edge pixel against a blown sky receives much of its light from a
/// sky that is several times brighter than the sensor's limit. That light
/// keeps the sky's real colour at full strength, and its interpolation used
/// clipped photosites that only bound their value from below. Highlight
/// reconstruction renders the clipped region itself towards neutral as its
/// channels run out, so without this the edge pixels show the colour the
/// region no longer has: pale cyan, blue or magenta specks. The brightest,
/// most completely clipped part of the source dominates the borrowed light,
/// and its reconstructed colour is neutral.
///
/// Within `spill` pixels of a pixel with a channel at the reconstruction
/// knee, the darkest unclipped pixel within `reach` estimates the covering
/// object. The share of a pixel's light above that level is treated as
/// borrowed and its colour moves to neutral; the channel sum is unchanged.
/// The darkest pixel misses sub-pixel sky in thin structures, so the share
/// is taken as its square root. Colour is kept where it is supported: by the
/// covering object (a red leaf), by the reconstructed source (a coloured
/// lamp), by unclipped surroundings beyond the spill (blue sky beside a
/// white cloud), or by being too strong for borrowed light or demosaicing
/// error. Frames without reconstructed pixels are returned bit for bit.
///
/// `reconstructed` flags the pixels highlight reconstruction worked on, in
/// this image's geometry (see `reconstructed_pixels`); lens correction's
/// vignetting gain can lift other pixels above the knee afterwards.
pub fn settle_borrowed_light(image: &mut Image, reconstructed: &[bool], spill: usize, reach: usize) {
    settle_stripes(image, reconstructed, spill, reach, 64);
}

/// Pixels with a channel at highlight reconstruction's knee, before any
/// gain is applied.
pub fn reconstructed_pixels(image: &Image, clip: [f32; 3]) -> Vec<bool> {
    let level = clip.map(|v| v * CLIP_KNEE);
    image.data.par_iter().map(|p| (0..3).any(|c| p[c] >= level[c])).collect()
}

fn settle_stripes(image: &mut Image, reconstructed: &[bool], spill: usize, reach: usize, stripe_rows: usize) {
    const THIRD: f32 = 1.0 / 3.0;
    let (w, h) = (image.width, image.height);
    assert_eq!(reconstructed.len(), w * h, "reconstruction flags must match the image");
    if spill == 0 || w == 0 || !reconstructed.par_iter().any(|&r| r) {
        return;
    }
    let halo = reach + spill;
    // Channel sum, red and blue chromaticity, and whether reconstructed.
    // A pixel without light has no hue: its chromaticity is neutral, so a
    // black covering object supports no colour.
    let encode = |(p, &flag): (&[f32; 3], &bool)| -> [f32; 4] {
        let sum = p[0].max(0.0) + p[1].max(0.0) + p[2].max(0.0);
        let (red, blue) = if sum > 1e-12 { (p[0].max(0.0) / sum, p[2].max(0.0) / sum) } else { (THIRD, THIRD) };
        [sum, red, blue, flag as u8 as f32]
    };
    // Chromaticity offset from neutral.
    let offset = |q: [f32; 4]| [q[1] - THIRD, 1.0 - q[1] - q[2] - THIRD, q[2] - THIRD];
    let ramp = |strength: f32, from: f32| {
        let t = ((strength - from) / from).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let length = |v: [f32; 3]| v.iter().map(|v| v * v).sum::<f32>().sqrt();
    // How much of `own` a reference colour explains, once the reference's
    // chroma reaches `from`.
    let supported = |own: [f32; 3], reference: [f32; 3], from: f32| {
        let own_power: f32 = own.iter().map(|v| v * v).sum();
        let along: f32 = own.iter().zip(reference).map(|(a, b)| a * b).sum();
        let explained = if own_power > 1e-12 { (along / own_power).clamp(0.0, 1.0) } else { 1.0 };
        explained * ramp(length(reference), from)
    };
    // Original encoded rows: results never read rows this pass has changed,
    // whatever the stripe size or scheduling.
    let mut source: Vec<[f32; 4]> = Vec::with_capacity(w * (stripe_rows + 2 * halo));
    let mut carried: Vec<[f32; 4]> = Vec::new();
    let mut counts: Vec<u32> = Vec::new();
    let mut across: Vec<bool> = Vec::new();
    let mut near: Vec<bool> = Vec::new();
    for first in (0..h).step_by(stripe_rows) {
        let end = (first + stripe_rows).min(h);
        let base = first.saturating_sub(halo);
        source.clear();
        source.extend_from_slice(&carried);
        let rows = first * w..(end + halo).min(h) * w;
        source.extend(image.data[rows.clone()].iter().zip(&reconstructed[rows]).map(encode));
        carried.clear();
        carried.extend_from_slice(&source[(end.saturating_sub(halo) - base) * w..(end - base) * w]);
        if source.iter().all(|q| q[3] == 0.0) {
            continue;
        }
        let rows = source.len() / w;
        // Pixels within `spill` of a reconstructed pixel, through per-row
        // prefix counts and a vertical pass.
        counts.clear();
        counts.resize(w + 1, 0);
        across.clear();
        across.resize(rows * w, false);
        for row in 0..rows {
            for x in 0..w {
                counts[x + 1] = counts[x] + source[row * w + x][3] as u32;
            }
            for x in 0..w {
                across[row * w + x] = counts[(x + spill).min(w - 1) + 1] != counts[x.saturating_sub(spill)];
            }
        }
        near.clear();
        near.resize(rows * w, false);
        near.par_chunks_mut(w).enumerate().for_each(|(row, flags)| {
            let (top, bottom) = (row.saturating_sub(spill), (row + spill).min(rows - 1));
            for (x, flag) in flags.iter_mut().enumerate() {
                *flag = (top..=bottom).any(|r| across[r * w + x]);
            }
        });
        let (source, near) = (&source, &near);
        image.data[first * w..end * w].par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let y = y + first - base;
            let (top, bottom) = (y.saturating_sub(reach), (y + reach).min(rows - 1));
            for (x, p) in row.iter_mut().enumerate() {
                let centre = source[y * w + x];
                if centre[3] != 0.0 || centre[0] <= 1e-12 || !near[y * w + x] {
                    continue;
                }
                let (left, right) = (x.saturating_sub(reach), (x + reach).min(w - 1));
                let mut darkest = centre;
                let (mut lamp, mut lit) = ([0.0f32; 3], 0.0f32);
                let (mut setting, mut light) = ([0.0f32; 3], 0.0f32);
                for r in top..=bottom {
                    for (i, &q) in source[r * w + left..=r * w + right].iter().enumerate() {
                        let o = offset(q);
                        if q[3] != 0.0 {
                            lamp = std::array::from_fn(|c| lamp[c] + o[c]);
                            lit += 1.0;
                            continue;
                        }
                        if q[0] < darkest[0] {
                            darkest = q;
                        }
                        if !near[r * w + left + i] {
                            setting = std::array::from_fn(|c| setting[c] + o[c] * q[0]);
                            light += q[0];
                        }
                    }
                }
                let own = offset(centre);
                // Strong colour is real (a lamp's glow); borrowed sky and
                // interpolation error stay at about half of it.
                let mut kept = ramp(length(own), 0.2).max(supported(own, offset(darkest), 0.1));
                if lit > 0.0 {
                    kept = kept.max(supported(own, lamp.map(|v| v / lit), 0.1));
                }
                if light > 1e-12 {
                    // The setting mixes in neutral twigs and highlights, so
                    // match its hue rather than its strength.
                    let setting = setting.map(|v| v / light);
                    let along: f32 = own.iter().zip(setting).map(|(a, b)| a * b).sum();
                    let cosine = (along / (length(own) * length(setting)).max(1e-12)).max(0.0);
                    kept = kept.max(cosine * cosine * ramp(length(setting), 0.05));
                }
                let share = ((centre[0] - darkest[0]) / centre[0]).clamp(0.0, 1.0).sqrt() * (1.0 - kept);
                if share <= 0.0 {
                    continue;
                }
                let red = centre[1] + share * (THIRD - centre[1]);
                let blue = centre[2] + share * (THIRD - centre[2]);
                *p = [red, 1.0 - red - blue, blue].map(|v| v * centre[0]);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arw;
    use crate::synthetic::Synthetic;

    #[test]
    fn clipped_thin_structure_keeps_measured_green_for_reconstruction() {
        let (w, h) = (80, 60);
        let clip = [2.5, 1.0, 1.6];
        let cfa = [0, 1, 1, 2];
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let d = (y as f32 - x as f32 - 1.0) / 2.0f32.sqrt();
                let v = 1.2 - 1.15 * (-0.5 * (d / 0.85).powi(2)).exp();
                v.min(clip[cfa[(y & 1) * 2 + (x & 1)] as usize])
            })
            .collect();
        let mosaic = Mosaic { width: w, height: h, data, cfa, clip };
        let mut image = full(&mosaic);
        for y in 0..h {
            for x in 0..w {
                if mosaic.color(x, y) == 1 {
                    assert_eq!(image.at(x, y)[1], mosaic.data[y * w + x]);
                }
            }
        }
        assert!(reconstruct_highlights(&mut image, clip) > 0.4);
        for y in 0..h {
            for x in 0..w {
                if mosaic.color(x, y) == 1 && mosaic.data[y * w + x] >= clip[1] * CLIP_MARGIN {
                    assert!(image.at(x, y)[1] >= mosaic.data[y * w + x]);
                }
            }
        }
    }

    #[test]
    fn neutral_diagonal_does_not_acquire_purple_at_either_size() {
        for cfa in [[0, 1, 1, 2], [2, 1, 1, 0], [1, 0, 2, 1], [1, 2, 0, 1]] {
            let (w, h) = (160, 120);
            let data = (0..w * h)
                .map(|i| {
                    let d = ((i / w) as f32 - (i % w) as f32 - 1.0) / 2.0f32.sqrt();
                    0.9 - 0.86 * (-0.5 * (d / 0.85).powi(2)).exp()
                })
                .collect();
            let mosaic = Mosaic { width: w, height: h, data, cfa, clip: [2.0; 3] };
            for image in [full(&mosaic), half(&mosaic)] {
                let mut sum = 0.0;
                let mut n = 0;
                for y in 8..image.height - 8 {
                    for x in 8..image.width - 8 {
                        let scale = w / image.width;
                        if ((y as isize - x as isize) * scale as isize - 1).abs() <= 3 {
                            let p = image.at(x, y);
                            sum += (p[0].min(p[2]) - p[1]).max(0.0);
                            n += 1;
                        }
                    }
                }
                assert!(sum / (n as f32) < 0.01, "CFA={cfa:?}, size={} excess={}", image.width, sum / n as f32);
            }
        }
    }

    #[test]
    fn neutral_half_refinement_does_not_hide_sensor_clipping() {
        let mosaic = Mosaic {
            width: 8,
            height: 8,
            data: (0..64).map(|i| if i % 8 < 4 { 0.3 } else { 1.0 }).collect(),
            cfa: [0, 1, 1, 2],
            clip: [2.5, 1.0, 1.6],
        };
        let mut image = half(&mosaic);
        assert_eq!(image.at(2, 2)[1], 1.0);
        assert!(reconstruct_highlights(&mut image, mosaic.clip) > 0.0);
    }

    #[test]
    fn refinement_handles_a_single_cfa_tile() {
        let mosaic =
            Mosaic { width: 2, height: 2, data: vec![0.9, 0.04, 0.04, 0.9], cfa: [0, 1, 1, 2], clip: [2.0; 3] };
        for im in [full(&mosaic), half(&mosaic)] {
            assert!(im.data.iter().flatten().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn a_highlight_with_two_clipped_channels_follows_the_surviving_one() {
        // Neutral ramp; green clips at 1.0 and blue at 1.6, red survives.
        let (w, h) = (200, 40);
        let clip = [2.5, 1.0, 1.6];
        let mut image = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = 0.3 + 1.9 * x as f32 / w as f32;
                image.data[y * w + x] = [v.min(clip[0]), v.min(clip[1]), v.min(clip[2])];
            }
        }
        reconstruct_highlights(&mut image, clip);
        let p = image.data[20 * w + 180]; // true value 2.01 in every channel
        assert!((p[1] - p[0]).abs() < 0.15 && (p[2] - p[0]).abs() < 0.15, "{p:?}");
    }

    #[test]
    fn coloured_highlight_has_no_jump_when_the_last_channel_clips() {
        let clip = [2.5f32, 1.0, 1.6];
        let mut image = Image::new(4000, 2);
        for y in 0..2 {
            for x in 0..4000 {
                let v = 0.3 + x as f32 * 0.0007;
                image.data[y * 4000 + x] = [(v * 1.15).min(clip[0]), v.min(clip[1]), (v * 0.8).min(clip[2])];
            }
        }
        reconstruct_highlights(&mut image, clip);
        let max_jump = image.data[..4000]
            .windows(2)
            .flat_map(|p| (0..3).map(move |c| (p[0][c] - p[1][c]).abs()))
            .fold(0.0f32, f32::max);
        assert!(max_jump < 0.01, "highlight boundary jumps by {max_jump}");
    }

    #[test]
    fn stuck_photosites_are_repaired_and_real_detail_is_kept() {
        let (w, h) = (64, 48);
        let mut data = vec![600u16; w * h];
        data[20 * w + 30] = 9000; // hot
        data[10 * w + 11] = 0; // dead (below black)
                               // A genuine bright feature spans neighbouring photosites.
        for y in 30..35 {
            for x in 40..45 {
                data[y * w + x] = 7000;
            }
        }
        let raw = arw::decode(&Synthetic::new(w, h, data).to_bytes()).unwrap();
        let m = prepare(&raw, [1.0; 3]);
        let level = (600.0 - 512.0) / (16383.0 - 512.0);
        assert!((m.data[20 * w + 30] - level).abs() < 1e-5, "hot photosite kept: {}", m.data[20 * w + 30]);
        assert!((m.data[10 * w + 11] - level).abs() < 1e-5, "dead photosite kept");
        assert!(m.data[32 * w + 42] > 0.3 && m.data[30 * w + 40] > 0.3, "real detail was removed");
    }

    #[test]
    fn flat_colour_survives_demosaicing_exactly() {
        let truth = [0.3f32, 0.5, 0.2];
        let data = (0..32 * 32).map(|i| truth[[0, 1, 1, 2][((i / 32) & 1) * 2 + (i & 1)]]).collect();
        let m = Mosaic { width: 32, height: 32, data, cfa: [0, 1, 1, 2], clip: [f32::MAX; 3] };
        for image in [full(&m), half(&m)] {
            for p in &image.data {
                for c in 0..3 {
                    assert!((p[c] - truth[c]).abs() < 1e-5);
                }
            }
        }
    }

    #[test]
    fn borrowed_light_next_to_a_blown_sky_loses_its_tint_but_real_colour_stays() {
        let clip = [2.5, 1.0, 1.6];
        let (w, h) = (64, 40);
        let sky = [3.0f32; 3];
        let twig = |x: usize| match x {
            22 | 26 => Some([1.2, 0.8, 1.3]),
            23 | 25 => Some([0.5, 0.35, 0.55]),
            24 => Some([0.2, 0.2, 0.2]),
            _ => None,
        };
        // A red leaf with a brighter rim where it meets the sky.
        let leaf = |x: usize, y: usize| match (x, y) {
            (41..=46, 21..=26) => Some([0.6, 0.1, 0.08]),
            (40..=47, 20..=27) => Some([1.0, 0.3, 0.25]),
            _ => None,
        };
        let mut image = Image::new(w, h);
        image.map_xy(|x, y, _| twig(x).or(leaf(x, y)).unwrap_or(sky));
        let original = image.clone();
        let mut striped = image.clone();
        let flags = reconstructed_pixels(&image, clip);
        settle_borrowed_light(&mut image, &flags, BORROWED_SPILL, BORROWED_REACH);
        settle_stripes(&mut striped, &flags, BORROWED_SPILL, BORROWED_REACH, 7);
        assert!(image.data == striped.data, "stripes must match one pass");

        let tint = |p: [f32; 3]| {
            let sum = p.iter().sum::<f32>();
            p.iter().map(|v| (v / sum - 1.0 / 3.0).abs()).fold(0.0, f32::max)
        };
        for y in 0..h {
            for x in 0..w {
                let (before, after) = (original.at(x, y), image.at(x, y));
                let sum = |p: [f32; 3]| p.iter().sum::<f32>();
                assert!((sum(before) - sum(after)).abs() < 1e-5 * sum(before));
                if matches!(x, 22 | 23 | 25 | 26) && !(20..=27).contains(&y) {
                    assert!(tint(after) < 0.3 * tint(before), "{x},{y}: {before:?} -> {after:?}");
                }
                if leaf(x, y).is_some() {
                    assert!(tint(after) > 0.9 * tint(before), "{x},{y}: {before:?} -> {after:?}");
                }
            }
        }

        // A black twig is no evidence of colour: its cyan edges still settle.
        let mut dark = Image::new(w, h);
        dark.map_xy(|x, _, _| match x {
            30 => [0.0; 3],
            29 | 31 => [0.3, 0.5, 0.5],
            _ => sky,
        });
        let before = dark.clone();
        let flags = reconstructed_pixels(&dark, clip);
        settle_borrowed_light(&mut dark, &flags, BORROWED_SPILL, BORROWED_REACH);
        for y in 0..h {
            for x in [29, 31] {
                assert!(tint(dark.at(x, y)) < 0.3 * tint(before.at(x, y)), "{x},{y}: {:?}", dark.at(x, y));
            }
        }

        // Without reconstructed pixels nothing is borrowed.
        let mut dim = original.clone();
        dim.map(|p| p.map(|v| v * 0.25));
        let reference = dim.clone();
        let flags = reconstructed_pixels(&dim, clip);
        settle_borrowed_light(&mut dim, &flags, BORROWED_SPILL, BORROWED_REACH);
        assert!(dim.data == reference.data);

        // A gain applied after reconstruction (lens vignetting) does not
        // make bright unclipped pixels into sources: only the flags do.
        let mut lifted = dim.clone();
        lifted.map(|p| p.map(|v| v * 4.0));
        let reference = lifted.clone();
        settle_borrowed_light(&mut lifted, &flags, BORROWED_SPILL, BORROWED_REACH);
        assert!(lifted.data == reference.data);
    }
}

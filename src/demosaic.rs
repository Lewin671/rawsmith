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

pub fn prepare(raw: &RawImage, wb: [f32; 3]) -> Mosaic {
    let crop = raw.crop;
    let mut data = vec![0.0f32; crop.width * crop.height];
    let scale: Vec<f32> = (0..4).map(|i| wb[raw.cfa[i] as usize] / (raw.white - raw.black[i]).max(1.0)).collect();
    data.par_chunks_mut(crop.width).enumerate().for_each(|(y, row)| {
        let sy = y + crop.y;
        let src = &raw.data[sy * raw.width + crop.x..sy * raw.width + crop.x + crop.width];
        for (x, (out, &v)) in row.iter_mut().zip(src).enumerate() {
            let cell = (sy & 1) * 2 + ((x + crop.x) & 1);
            *out = (v as f32 - raw.black[cell]) * scale[cell];
        }
    });
    // The crop origin is even, so the CFA tile is unchanged.
    Mosaic { width: crop.width, height: crop.height, data, cfa: raw.cfa, clip: wb }
}

/// Half-resolution demosaic: each 2x2 CFA tile becomes one pixel. Fast and
/// free of interpolation artefacts; used for previews and analysis.
pub fn half(m: &Mosaic) -> Image {
    let (w, h) = (m.width / 2, m.height / 2);
    let mut out = Image::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let mut sum = [0.0f32; 3];
            let mut saturated = [false; 3];
            for cell in 0..4 {
                let c = m.cfa[cell] as usize;
                let v = m.data[(y * 2 + cell / 2) * m.width + x * 2 + cell % 2];
                sum[c] += v;
                saturated[c] |= v >= m.clip[c] * CLIP_MARGIN;
            }
            // A green that averages one clipped and one unclipped sample must
            // still read as clipped for highlight reconstruction.
            let g = if saturated[1] { m.clip[1] } else { sum[1] * 0.5 };
            *p = [sum[0], g, sum[2]];
        }
    });
    out
}

/// Mirror an index into `0..n`, preserving its parity (and so its CFA colour).
#[inline]
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    (if i < 0 {
        -i
    } else if i >= n {
        2 * n - 2 - i
    } else {
        i
    }) as usize
}

/// Full-resolution demosaic: edge-directed green interpolation with Laplacian
/// correction (Hamilton-Adams, soft-weighted), then red and blue from
/// interpolated colour differences.
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
const CLIP_MARGIN: f32 = 0.98;

/// Rebuilds colour channels that saturated on the sensor.
///
/// A clipped channel only says "at least this much". Its true value is
/// estimated from the channels that did not clip plus the typical offset
/// between them, measured on pixels just outside the clipped areas. Without
/// this, blown highlights turn magenta once white balance scales the channels
/// apart. Returns the fraction of pixels that had at least one clipped channel.
pub fn reconstruct_highlights(image: &mut Image, clip: [f32; 3]) -> f32 {
    let level = [clip[0] * CLIP_MARGIN, clip[1] * CLIP_MARGIN, clip[2] * CLIP_MARGIN];
    let (w, h) = (image.width, image.height);
    let clipped: Vec<u8> =
        image.data.par_iter().map(|p| (0..3).fold(0u8, |mask, c| mask | (((p[c] >= level[c]) as u8) << c))).collect();
    let count = clipped.par_iter().filter(|&&m| m != 0).count();
    if count == 0 {
        return 0.0;
    }

    // Offsets are measured in cube-root space, where they are roughly
    // independent of brightness.
    let opposed = |p: [f32; 3], c: usize| {
        let (a, b) = (p[(c + 1) % 3].max(0.0).cbrt(), p[(c + 2) % 3].max(0.0).cbrt());
        (a + b) * 0.5
    };
    let reach = (w.max(h) / 400).max(2) as isize;
    let sums = (0..h)
        .into_par_iter()
        .map(|y| {
            let mut acc = [(0.0f64, 0u64); 3];
            for x in 0..w {
                let p = image.data[y * w + x];
                if clipped[y * w + x] != 0 || p.iter().any(|&v| v < 0.2 * clip[1]) {
                    continue;
                }
                // Bright, unclipped, and close to a clipped pixel.
                let mut near = 0u8;
                for (dx, dy) in [(-reach, 0), (reach, 0), (0, -reach), (0, reach)] {
                    let (nx, ny) = (x as isize + dx, y as isize + dy);
                    if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                        near |= clipped[ny as usize * w + nx as usize];
                    }
                }
                for c in 0..3 {
                    if near & (1 << c) != 0 {
                        acc[c].0 += (p[c].cbrt() - opposed(p, c)) as f64;
                        acc[c].1 += 1;
                    }
                }
            }
            acc
        })
        .reduce(
            || [(0.0, 0); 3],
            |a, b| {
                [
                    (a[0].0 + b[0].0, a[0].1 + b[0].1),
                    (a[1].0 + b[1].0, a[1].1 + b[1].1),
                    (a[2].0 + b[2].0, a[2].1 + b[2].1),
                ]
            },
        );
    let offset: Vec<f32> = sums.iter().map(|&(s, n)| if n > 100 { (s / n as f64) as f32 } else { 0.0 }).collect();

    image.data.par_iter_mut().zip(&clipped).for_each(|(p, &mask)| {
        if mask == 0 {
            return;
        }
        let original = *p;
        if mask == 0b111 {
            // Nothing left to infer colour from: a neutral highlight at the
            // brightest channel's level.
            *p = [original[0].max(original[1]).max(original[2]); 3];
            return;
        }
        for c in 0..3 {
            if mask & (1 << c) != 0 {
                let estimate = (opposed(original, c) + offset[c]).max(0.0).powi(3);
                p[c] = original[c].max(estimate);
            }
        }
    });
    count as f32 / (w * h) as f32
}

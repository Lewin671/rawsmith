//! Float RGB image buffer and the resampling / filtering primitives the
//! pipeline shares.

use rayon::prelude::*;

fn resize_spans(from: usize, to: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = from as f64 / to as f64;
    (0..to)
        .map(|i| {
            let (a, b) = (i as f64 * scale, (i + 1) as f64 * scale);
            let first = a.floor() as usize;
            let last = ((b.ceil() as usize).max(first + 1)).min(from);
            let weights = (first..last).map(|s| ((b.min((s + 1) as f64) - a.max(s as f64)) / scale) as f32).collect();
            (first, weights)
        })
        .collect()
}

#[derive(Clone)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub data: Vec<[f32; 3]>,
}

impl Image {
    pub fn new(width: usize, height: usize) -> Self {
        Image { width, height, data: vec![[0.0; 3]; width * height] }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> [f32; 3] {
        self.data[y * self.width + x]
    }

    /// Bilinear sample at continuous pixel-centre coordinates, clamped to the edge.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let x = x.clamp(0.0, (self.width - 1) as f32);
        let y = y.clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let (a, b, c, d) = (self.at(x0, y0), self.at(x1, y0), self.at(x0, y1), self.at(x1, y1));
        let mut out = [0.0; 3];
        for i in 0..3 {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bottom = c[i] + (d[i] - c[i]) * fx;
            out[i] = top + (bottom - top) * fy;
        }
        out
    }

    /// Applies `f` to every pixel in parallel.
    pub fn map(&mut self, f: impl Fn([f32; 3]) -> [f32; 3] + Sync) {
        self.data.par_iter_mut().for_each(|p| *p = f(*p));
    }

    /// Applies `f(x, y, pixel)` to every pixel in parallel.
    pub fn map_xy(&mut self, f: impl Fn(usize, usize, [f32; 3]) -> [f32; 3] + Sync) {
        let width = self.width;
        self.data.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
            for (x, p) in row.iter_mut().enumerate() {
                *p = f(x, y, *p);
            }
        });
    }

    pub fn crop(&self, x: usize, y: usize, width: usize, height: usize) -> Image {
        let mut out = Image::new(width, height);
        for row in 0..height {
            let from = (y + row) * self.width + x;
            out.data[row * width..(row + 1) * width].copy_from_slice(&self.data[from..from + width]);
        }
        out
    }

    /// Applies an EXIF orientation (only the rotations cameras actually write).
    pub fn oriented(self, orientation: u16) -> Image {
        let (w, h) = (self.width, self.height);
        match orientation {
            3 => {
                let mut data = self.data;
                data.reverse();
                Image { width: w, height: h, data }
            }
            6 | 8 => {
                let mut out = Image::new(h, w);
                let src = &self.data;
                out.data.par_chunks_mut(h).enumerate().for_each(|(y, row)| {
                    for (x, p) in row.iter_mut().enumerate() {
                        // 6: rotate 90 degrees clockwise; 8: counter-clockwise.
                        *p = if orientation == 6 { src[(h - 1 - x) * w + y] } else { src[x * w + (w - 1 - y)] };
                    }
                });
                out
            }
            _ => self,
        }
    }

    /// Area-averaging resize, exact for any ratio. Used for downscaling,
    /// where it is alias-free; for upscaling it degrades to nearest-neighbour.
    pub fn resized(&self, width: usize, height: usize) -> Image {
        if width == self.width && height == self.height {
            return self.clone();
        }
        let (xs, ys) = (resize_spans(self.width, width), resize_spans(self.height, height));
        // Horizontal pass, then vertical.
        let mut mid = vec![[0.0f32; 3]; width * self.height];
        mid.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
            let src = &self.data[y * self.width..(y + 1) * self.width];
            for (p, (first, weights)) in row.iter_mut().zip(&xs) {
                for (k, w) in weights.iter().enumerate() {
                    let s = src[first + k];
                    for c in 0..3 {
                        p[c] += s[c] * w;
                    }
                }
            }
        });
        let mut out = Image::new(width, height);
        out.data.par_chunks_mut(width).zip(&ys).for_each(|(row, (first, weights))| {
            for (k, w) in weights.iter().enumerate() {
                let src = &mid[(first + k) * width..(first + k + 1) * width];
                for (p, s) in row.iter_mut().zip(src) {
                    for c in 0..3 {
                        p[c] += s[c] * w;
                    }
                }
            }
        });
        out
    }
}

/// Single-channel plane used for masks and luminance work.
#[derive(Clone)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Plane {
    pub fn new(width: usize, height: usize) -> Self {
        Plane { width, height, data: vec![0.0; width * height] }
    }

    pub fn from_fn(image: &Image, f: impl Fn([f32; 3]) -> f32 + Sync) -> Self {
        Plane { width: image.width, height: image.height, data: image.data.par_iter().map(|p| f(*p)).collect() }
    }

    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.width - 1) as f32);
        let y = y.clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let at = |x: usize, y: usize| self.data[y * self.width + x];
        let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
        let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
        top + (bottom - top) * fy
    }

    pub fn resized(&self, width: usize, height: usize) -> Plane {
        if (width, height) == (self.width, self.height) {
            return self.clone();
        }
        let (xs, ys) = (resize_spans(self.width, width), resize_spans(self.height, height));
        let mut mid = vec![0.0f32; width * self.height];
        mid.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
            let src = &self.data[y * self.width..(y + 1) * self.width];
            for (p, (first, weights)) in row.iter_mut().zip(&xs) {
                for (k, w) in weights.iter().enumerate() {
                    *p += src[first + k] * w;
                }
            }
        });
        let mut out = Plane::new(width, height);
        out.data.par_chunks_mut(width).zip(&ys).for_each(|(row, (first, weights))| {
            for (k, w) in weights.iter().enumerate() {
                let src = &mid[(first + k) * width..(first + k + 1) * width];
                for (p, s) in row.iter_mut().zip(src) {
                    *p += s * w;
                }
            }
        });
        out
    }

    /// Box blur with the given radius (window `2r + 1`), edges replicated.
    pub fn box_blur(&self, radius: usize) -> Plane {
        if radius == 0 {
            return self.clone();
        }
        let (w, h) = (self.width, self.height);
        let norm = 1.0 / (2 * radius + 1) as f32;
        let run = |src: &[f32], dst: &mut [f32], n: usize, stride: usize| {
            let get = |i: isize| src[(i.clamp(0, n as isize - 1) as usize) * stride];
            let mut sum: f32 = (-(radius as isize)..=radius as isize).map(get).sum();
            for i in 0..n {
                dst[i * stride] = sum * norm;
                sum += get(i as isize + radius as isize + 1) - get(i as isize - radius as isize);
            }
        };
        let mut mid = vec![0.0f32; w * h];
        mid.par_chunks_mut(w).zip(self.data.par_chunks(w)).for_each(|(dst, src)| run(src, dst, w, 1));
        Self::box_columns(w, h, radius, mid, vec![0.0f32; w * h])
    }

    /// Consume a disposable plane so its storage can hold the column pass.
    pub(crate) fn box_blur_owned(self, radius: usize) -> Plane {
        if radius == 0 {
            return self;
        }
        let (w, h) = (self.width, self.height);
        let norm = 1.0 / (2 * radius + 1) as f32;
        let mut mid = vec![0.0f32; w * h];
        mid.par_chunks_mut(w).zip(self.data.par_chunks(w)).for_each(|(dst, src)| {
            let get = |i: isize| src[i.clamp(0, w as isize - 1) as usize];
            let mut sum: f32 = (-(radius as isize)..=radius as isize).map(get).sum();
            for (i, v) in dst.iter_mut().enumerate() {
                *v = sum * norm;
                sum += get(i as isize + radius as isize + 1) - get(i as isize - radius as isize);
            }
        });
        Self::box_columns(w, h, radius, mid, self.data)
    }

    fn box_columns(w: usize, h: usize, radius: usize, mut mid: Vec<f32>, mut columns: Vec<f32>) -> Plane {
        // Neighbouring columns share contiguous source loads while keeping
        // each column's original top-to-bottom floating-point sum order.
        const TILE: usize = 32;
        let norm = 1.0 / (2 * radius + 1) as f32;
        columns.par_chunks_mut(h * TILE).enumerate().for_each(|(tile, cols)| {
            let x0 = tile * TILE;
            let n = cols.len() / h;
            let mut sums = [-0.0f32; TILE];
            let row_at = |y: isize| {
                let y = y.clamp(0, h as isize - 1) as usize;
                &mid[y * w + x0..y * w + x0 + n]
            };
            for y in -(radius as isize)..=radius as isize {
                for (sum, v) in sums[..n].iter_mut().zip(row_at(y)) {
                    *sum += v;
                }
            }
            for y in 0..h {
                let added = row_at(y as isize + radius as isize + 1);
                let removed = row_at(y as isize - radius as isize);
                let row = &mut cols[y * n..(y + 1) * n];
                for (((v, sum), added), removed) in row.iter_mut().zip(&mut sums[..n]).zip(added).zip(removed) {
                    *v = *sum * norm;
                    *sum += added - removed;
                }
            }
        });
        // The horizontal buffer is dead; transpose back into its storage.
        mid.par_chunks_mut(w * TILE).enumerate().for_each(|(tile, rows)| {
            let y0 = tile * TILE;
            for (x_tile, cols) in columns.chunks(h * TILE).enumerate() {
                let n = cols.len() / h;
                let x0 = x_tile * TILE;
                for (y, row) in rows.chunks_mut(w).enumerate() {
                    row[x0..x0 + n].copy_from_slice(&cols[(y0 + y) * n..(y0 + y + 1) * n]);
                }
            }
        });
        Plane { width: w, height: h, data: mid }
    }

    /// Approximate Gaussian blur: three box blurs of matching variance.
    pub fn blur(&self, sigma: f32) -> Plane {
        if sigma < 0.3 {
            return self.clone();
        }
        // Variance of a box of radius r is r(r+1)/3; three passes add up.
        let radius = (((sigma * sigma + 0.25).sqrt() - 0.5).round() as usize).max(1);
        self.box_blur(radius).box_blur_owned(radius).box_blur_owned(radius)
    }
}

/// Edge-preserving smoothing of `input` guided by `guide` (He et al., guided
/// filter). `radius` is the window radius, `eps` the edge threshold in
/// squared guide units.
/// One level of the undecimated B3-spline wavelet transform: a 1-4-6-4-1
/// smoothing with `step - 1` holes between taps, mirrored at the borders.
fn atrous_smooth(input: &Plane, step: usize) -> Plane {
    const K: [f32; 3] = [6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let (w, h) = (input.width, input.height);
    let mirror = |i: isize, n: usize| {
        if n == 1 {
            return 0;
        }
        let period = (2 * n - 2) as isize;
        let i = i.rem_euclid(period);
        (if i >= n as isize { period - i } else { i }) as usize
    };
    let tap = |row: &[f32], x: usize| {
        let at = |offset: isize| row[mirror(x as isize + offset * step as isize, w)];
        K[0] * row[x] + K[1] * (at(-1) + at(1)) + K[2] * (at(-2) + at(2))
    };
    let mut rows = Plane::new(w, h);
    rows.data.par_chunks_mut(w).zip(input.data.par_chunks(w)).for_each(|(out, row)| {
        // Only the margins need mirrored indices; the interior is a plain
        // five-tap sum over fixed offsets.
        let margin = (2 * step).min(w);
        if w > 2 * margin {
            let interior = w - 2 * margin;
            let (far_left, left, centre, right, far_right) = (
                &row[..interior],
                &row[step..step + interior],
                &row[margin..margin + interior],
                &row[margin + step..margin + step + interior],
                &row[2 * margin..],
            );
            for (i, v) in out[margin..w - margin].iter_mut().enumerate() {
                *v = K[0] * centre[i] + K[1] * (left[i] + right[i]) + K[2] * (far_left[i] + far_right[i]);
            }
            for x in (0..margin).chain(w - margin..w) {
                out[x] = tap(row, x);
            }
        } else {
            for (x, v) in out.iter_mut().enumerate() {
                *v = tap(row, x);
            }
        }
    });
    let mut out = Plane::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = |offset: isize| {
            let y = mirror(y as isize + offset * step as isize, h);
            &rows.data[y * w..(y + 1) * w]
        };
        let (far_up, up, centre, down, far_down) = (row(-2), row(-1), row(0), row(1), row(2));
        for (x, v) in out.iter_mut().enumerate() {
            *v = K[0] * centre[x] + K[1] * (up[x] + down[x]) + K[2] * (far_up[x] + far_down[x]);
        }
    });
    out
}

/// Largest absolute mean of `detail` along a straight segment of
/// `2 * radius + 1` taps, `step` apart, through each pixel: horizontal,
/// vertical and both diagonals. Samples past the border repeat the edge.
fn line_response(detail: &Plane, step: usize, radius: usize) -> Plane {
    let (w, h) = (detail.width, detail.height);
    let d = &detail.data;
    let at = |x: isize, y: isize| d[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];
    let direct = |x: usize, y: usize, dx: isize, dy: isize| {
        let r = radius as isize;
        (-r..=r).map(|i| at(x as isize + i * dx * step as isize, y as isize + i * dy * step as isize)).sum::<f32>()
    };
    // Running sums: each segment is the one `step` back along its direction
    // plus the sample entering and minus the one leaving. Where a segment
    // would reach past the edge it is summed directly. Bands restart the
    // sums so they run in parallel with a fixed order.
    let (reach, back) = (radius * step, (radius + 1) * step);
    // Interior columns, whose segments and predecessors stay inside the row.
    let (lo, hi) = (back.min(w), w.saturating_sub(back).max(back.min(w)));
    const BAND: usize = 128;
    let mut out = Plane::new(w, h);
    out.data.par_chunks_mut(w * BAND).enumerate().for_each(|(band, rows)| {
        let count = rows.len() / w;
        let mut previous = vec![vec![0.0f32; step * w]; 3];
        let mut current = vec![0.0f32; w];
        let mut horizontal = vec![0.0f32; w];
        for r in 0..count {
            let y = band * BAND + r;
            let row = &mut rows[r * w..(r + 1) * w];
            let line = &d[y * w..(y + 1) * w];
            for x in (0..lo).chain(hi..w) {
                row[x] = direct(x, y, 1, 0);
            }
            // Each residue of x modulo `step` is its own running sum, seeded
            // by the directly summed segment just before the interior.
            for start in (lo..hi).take(step) {
                let mut sum = row[start - step];
                for x in (start..hi).step_by(step) {
                    sum += line[x + reach] - line[x - back];
                    row[x] = sum;
                }
            }
            for (m, v) in horizontal.iter_mut().zip(row.iter()) {
                *m = v.abs();
            }
            let running = r >= step && y >= back && y + reach < h;
            for (k, dx) in [0isize, 1, -1].into_iter().enumerate() {
                let slot = (r % step) * w;
                if running && hi > lo {
                    let (add, remove) =
                        (&d[(y + reach) * w..(y + reach + 1) * w], &d[(y - back) * w..(y - back + 1) * w]);
                    let prev = &previous[k][slot..slot + w];
                    let offset = |o: usize| (lo as isize + dx * o as isize) as usize;
                    let against = |o: usize| (lo as isize - dx * o as isize) as usize;
                    let n = hi - lo;
                    let (prev, add, remove) =
                        (&prev[against(step)..][..n], &add[offset(reach)..][..n], &remove[against(back)..][..n]);
                    for (((c, p), a), m) in current[lo..hi].iter_mut().zip(prev).zip(add).zip(remove) {
                        *c = p + a - m;
                    }
                    for x in (0..lo).chain(hi..w) {
                        current[x] = direct(x, y, dx, 1);
                    }
                } else {
                    for (x, c) in current.iter_mut().enumerate() {
                        *c = direct(x, y, dx, 1);
                    }
                }
                for (o, c) in horizontal.iter_mut().zip(&current) {
                    *o = o.max(c.abs());
                }
                previous[k][slot..slot + w].copy_from_slice(&current);
            }
            let n = (2 * radius + 1) as f32;
            for (v, m) in row.iter_mut().zip(&horizontal) {
                *v = m / n;
            }
        }
    });
    out
}

/// Wavelet shrinkage of square-root encoded luminance, where sensor noise
/// is roughly uniform. `sigma` is the noise in the finest of four scales;
/// coarser scales hold the fixed shares that demosaiced sensor noise leaves
/// there. Each scale removes coefficients within `threshold` noise levels
/// and keeps those well above, so grain goes at every size while edges and
/// firm texture stay. The noise level comes from the sensor measurement, not
/// from this picture's detail: fine texture is never mistaken for more noise.
pub fn wavelet_denoise(input: Plane, sigma: f32, threshold: f32) -> Plane {
    if sigma <= 0.0 || threshold <= 0.0 {
        return input;
    }
    // Measured on quiet areas of ISO 100-6400 frames after demosaicing: band
    // noise relative to the finest band. The finest band also carries the
    // isolated extremes that survive as specks, so it is thresholded harder.
    const NOISE: [f32; 4] = [1.0, 0.46, 0.22, 0.12];
    const WEIGHT: [f32; 4] = [1.5, 1.1, 0.8, 0.6];
    // Noise left in each smoothed plane, for judging its gradients.
    const RESIDUAL: [f32; 4] = [0.53, 0.26, 0.145, 0.08];
    let (w, h) = (input.width, input.height);
    let mut kept = Plane::new(w, h);
    let mut current = input;
    for scale in 0..4 {
        let step = 1usize << scale;
        let smooth = atrous_smooth(&current, step);
        let noise = (sigma * NOISE[scale]).powi(2);
        let ceiling = (threshold * WEIGHT[scale]).powi(2) * noise;
        // A difference of two smoothed samples carries sqrt(2) of their noise.
        let slope_noise = std::f32::consts::SQRT_2 * RESIDUAL[scale] * sigma;
        // Local energy of this band. Where it clearly exceeds the noise there
        // is texture, and the threshold falls to noise^2 / signal (BayesShrink)
        // so low-contrast detail is kept; in flat areas it stays at its
        // ceiling, which also removes the isolated extremes seen as specks.
        let mut energy = Plane::new(w, h);
        current.data.par_iter_mut().zip(&smooth.data).zip(&mut energy.data).for_each(|((c, s), e)| {
            *c -= *s;
            *e = *c * *c;
        });
        let energy = energy.box_blur_owned(3 * step);
        // Thin trunks, branches and feather barbs are too faint per sample
        // to clear the threshold, but they run straight for many samples:
        // averaged along their direction they stand above the noise, which
        // averages away. Such coefficients are kept like an edge. For
        // demosaiced grain the best of four 17-tap means exceeds 0.9 band
        // noise levels at about 2-4% of samples and 1.4 almost nowhere.
        // The coarsest band is left alone: its segments would span 64 pixels.
        let lines = (scale < 3).then(|| line_response(&current, step, 8));
        let band_noise = sigma * NOISE[scale];
        kept.data.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
            let row = |y: usize| &smooth.data[y * w..(y + 1) * w];
            let (here, above, below) = (row(y), row(y.saturating_sub(step)), row((y + step).min(h - 1)));
            for (x, out) in out.iter_mut().enumerate() {
                let d = current.data[y * w + x];
                // The margin keeps chance fluctuations of pure noise at the ceiling.
                let signal = (energy.data[y * w + x] - 2.0 * noise).max(0.0);
                let limit = if signal > 0.0 { (noise * noise / signal).min(ceiling) } else { ceiling };
                // Real structure persists in the smoother plane; noise does
                // not. Where that plane has a clear slope, an edge is passing
                // through and its detail is left alone.
                let slope =
                    (here[(x + step).min(w - 1)] - here[x.saturating_sub(step)]).abs().max((below[x] - above[x]).abs());
                let t = ((slope / slope_noise - 2.5) / 2.5).clamp(0.0, 1.0);
                let limit = limit * (1.0 - t * t * (3.0 - 2.0 * t));
                let limit = match &lines {
                    Some(lines) => {
                        let t = ((lines.data[y * w + x] / band_noise - 0.9) / 0.5).clamp(0.0, 1.0);
                        limit * (1.0 - t * t * (3.0 - 2.0 * t))
                    }
                    None => limit,
                };
                // Non-negative garrote: nothing below the threshold, nearly all of a strong edge.
                let d2 = d * d;
                if d2 > limit {
                    *out += d * (1.0 - limit / d2.max(1e-20));
                }
            }
        });
        current = smooth;
    }
    kept.data.par_iter_mut().zip(&current.data).for_each(|(out, base)| *out += *base);
    kept
}

pub fn guided_filter(guide: &Plane, input: &Plane, radius: usize, eps: f32) -> Plane {
    Guided::new(guide, radius).into_filter(input.clone(), eps)
}

/// Reuse the guide's window statistics for multiple input channels.
pub(crate) struct Guided<'a> {
    guide: &'a Plane,
    radius: usize,
    mean: Plane,
    correlation: Plane,
}
impl<'a> Guided<'a> {
    pub(crate) fn new(guide: &'a Plane, radius: usize) -> Self {
        let squared =
            Plane { width: guide.width, height: guide.height, data: guide.data.par_iter().map(|v| v * v).collect() };
        Self { guide, radius, mean: guide.box_blur(radius), correlation: squared.box_blur_owned(radius) }
    }

    pub(crate) fn filter_owned(&self, input: Plane, eps: f32) -> Plane {
        let (a, b) = self.coefficients(input, eps);
        Self::reconstruct(self.guide, self.radius, a, b)
    }

    pub(crate) fn into_filter(self, input: Plane, eps: f32) -> Plane {
        let (a, b) = self.coefficients(input, eps);
        let (guide, radius) = (self.guide, self.radius);
        drop(self);
        Self::reconstruct(guide, radius, a, b)
    }

    fn coefficients(&self, mut input: Plane, eps: f32) -> (Plane, Plane) {
        // A flat guide can have negative variance from cancellation.
        let eps = eps.max(f32::EPSILON);
        let mut b = input.box_blur(self.radius);
        input.data.par_iter_mut().zip(&self.guide.data).for_each(|(v, g)| {
            let product = *g * *v;
            *v = product;
        });
        let mut a = input.box_blur_owned(self.radius);
        a.data.par_iter_mut().zip(b.data.par_iter_mut()).enumerate().for_each(|(i, (a, b))| {
            let var = (self.correlation.data[i] - self.mean.data[i] * self.mean.data[i]).max(0.0);
            let cov = *a - self.mean.data[i] * *b;
            *a = cov / (var + eps);
            *b -= *a * self.mean.data[i];
        });
        (a, b)
    }

    fn reconstruct(guide: &Plane, radius: usize, a: Plane, b: Plane) -> Plane {
        let mean_a = a.box_blur_owned(radius);
        let mean_b = b.box_blur_owned(radius);
        // Reuse one coefficient plane for the final reconstruction.
        let mut out = mean_a;
        out.data.par_iter_mut().enumerate().for_each(|(i, v)| *v = *v * guide.data[i] + mean_b.data[i]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic unit-variance noise, spatially correlated like demosaiced grain.
    fn grain(width: usize, height: usize, seed: u64) -> Plane {
        let mut state = seed;
        let mut next = || {
            let sum: f32 = (0..12)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state >> 40) as f32 / (1u64 << 24) as f32
                })
                .sum();
            sum - 6.0
        };
        let white = Plane { width, height, data: (0..width * height).map(|_| next()).collect() }.blur(0.6);
        let rms = (white.data.iter().map(|v| v * v).sum::<f32>() / white.data.len() as f32).sqrt();
        Plane { data: white.data.iter().map(|v| v / rms).collect(), ..white }
    }

    fn rms(values: impl Iterator<Item = f32>) -> f32 {
        let (sum, count) = values.fold((0.0f64, 0usize), |(sum, count), v| (sum + (v * v) as f64, count + 1));
        (sum / count as f64).sqrt() as f32
    }

    #[test]
    fn wavelet_denoise_removes_grain_and_keeps_edges_and_firm_texture() {
        let (w, h, sigma) = (256, 192, 0.01f32);
        let noise = grain(w, h, 0x9E37_79B9);
        // Left: flat. Middle: a step edge. Right: texture peaking six noise levels high.
        let clean = Plane {
            width: w,
            height: h,
            data: (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let step = if x >= 96 { 0.12 } else { 0.0 };
                    let texture =
                        if x >= 160 { 6.0 * sigma * (x as f32 * 0.9).sin() * (y as f32 * 0.8).cos() } else { 0.0 };
                    0.3 + step + texture
                })
                .collect(),
        };
        let noisy =
            Plane { data: clean.data.iter().zip(&noise.data).map(|(c, n)| c + sigma * n).collect(), ..clean.clone() };
        // The finest band holds most, not all, of the grain.
        let smooth = wavelet_denoise(noisy.clone(), sigma * 0.89, 2.2);
        let region = |plane: &Plane, x0: usize, x1: usize| -> Vec<f32> {
            (16..h - 16).flat_map(|y| (x0..x1).map(move |x| y * w + x)).map(|i| plane.data[i] - clean.data[i]).collect()
        };
        let left = rms(region(&smooth, 16, 80).into_iter()) / rms(region(&noisy, 16, 80).into_iter());
        assert!(left < 0.5, "flat grain remaining: {left}");
        // The step keeps its height and steepness right at the edge.
        let column =
            |plane: &Plane, x: usize| (16..h - 16).map(|y| plane.data[y * w + x]).sum::<f32>() / (h - 32) as f32;
        let slope = (column(&smooth, 96) - column(&smooth, 95)) / 0.12;
        assert!((0.9..1.1).contains(&slope), "edge step kept: {slope}");
        // Texture above the noise survives: projection of the result on the true texture.
        let (mut cross, mut power) = (0.0f64, 0.0f64);
        for y in 16..h - 16 {
            for x in 176..w - 16 {
                let truth = (clean.data[y * w + x] - 0.42) as f64;
                cross += (smooth.data[y * w + x] - 0.42) as f64 * truth;
                power += truth * truth;
            }
        }
        assert!(cross / power > 0.9, "firm texture kept: {}", cross / power);
    }

    #[test]
    fn wavelet_denoise_is_exact_when_off_and_safe_on_tiny_or_flat_planes() {
        let noisy = grain(40, 30, 7);
        for (sigma, threshold) in [(0.0, 2.0), (0.01, 0.0)] {
            assert_eq!(wavelet_denoise(noisy.clone(), sigma, threshold).data, noisy.data);
        }
        for (w, h) in [(1, 1), (1, 9), (9, 1), (2, 3), (5, 5)] {
            let flat = Plane { width: w, height: h, data: vec![0.661_649_1; w * h] };
            let out = wavelet_denoise(flat, 0.01, 2.2);
            assert!(out.data.iter().all(|v| (v - 0.661_649_1).abs() < 1e-6), "{w}x{h}: {:?}", out.data);
        }
        let reference = wavelet_denoise(noisy.clone(), 0.5, 2.2);
        for threads in [1, 3] {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            assert_eq!(pool.install(|| wavelet_denoise(noisy.clone(), 0.5, 2.2)).data, reference.data);
        }
    }

    #[test]
    fn wavelet_denoise_keeps_faint_thin_lines_and_still_cleans_flat_grain() {
        // Thin dark trunks against fog: per pixel they are within the noise,
        // along their length they are not.
        let (w, h, sigma) = (256, 192, 0.01f32);
        let noise = grain(w, h, 0x5EED);
        // Position, width, depth in noise levels, and the share that must survive
        // (without the line test: 0.22, 0.38, 0.31).
        let lines = [(64usize, 1usize, 2.0f32, 0.7f32), (128, 2, 1.6, 0.48), (192, 3, 1.6, 0.4)];
        let clean = Plane {
            width: w,
            height: h,
            data: (0..w * h)
                .map(|i| {
                    let x = i % w;
                    0.3 - lines
                        .iter()
                        .filter(|(at, width, ..)| (*at..at + width).contains(&x))
                        .map(|l| l.2 * sigma)
                        .sum::<f32>()
                })
                .collect(),
        };
        let noisy =
            Plane { data: clean.data.iter().zip(&noise.data).map(|(c, n)| c + sigma * n).collect(), ..clean.clone() };
        let smooth = wavelet_denoise(noisy.clone(), sigma * 0.89, 2.2);
        let column =
            |plane: &Plane, x: usize| (16..h - 16).map(|y| plane.data[y * w + x]).sum::<f32>() / (h - 32) as f32;
        for (at, width, depth, floor) in lines {
            let dip = |plane: &Plane| {
                let line = (at..at + width).map(|x| column(plane, x)).sum::<f32>() / width as f32;
                let around = (column(plane, at - 6) + column(plane, at + width + 5)) * 0.5;
                around - line
            };
            let kept = dip(&smooth) / dip(&clean);
            eprintln!("line {width} px, {depth} noise levels deep: kept {kept:.3}");
            assert!(kept > floor, "line {width} px wide kept {kept}");
        }
        let flat = |plane: &Plane| {
            rms((16..h - 16).flat_map(|y| (16..48).map(move |x| y * w + x)).map(|i| plane.data[i] - 0.3))
        };
        let left = flat(&smooth) / flat(&noisy);
        assert!(left < 0.5, "flat grain remaining: {left}");
    }

    #[test]
    fn line_response_matches_direct_segment_sums() {
        for (w, h) in [(300, 260), (5, 300), (300, 7), (1, 1), (40, 40)] {
            let plane = Plane {
                width: w,
                height: h,
                data: (0..w * h).map(|i| ((i * 7919 + 13) % 1000) as f32 * 1e-3 - 0.5).collect(),
            };
            let at = |x: isize, y: isize| {
                plane.data[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize]
            };
            for step in [1, 2, 4] {
                let response = line_response(&plane, step, 8);
                for y in 0..h {
                    for x in 0..w {
                        let best = [(1isize, 0isize), (0, 1), (1, 1), (-1, 1)]
                            .iter()
                            .map(|&(dx, dy)| {
                                let (x, y, step) = (x as isize, y as isize, step as isize);
                                (-8..=8).map(|i| at(x + i * dx * step, y + i * dy * step)).sum::<f32>().abs() / 17.0
                            })
                            .fold(0.0, f32::max);
                        let got = response.data[y * w + x];
                        assert!((got - best).abs() < 1e-4, "{w}x{h} step {step} ({x}, {y}): {got} against {best}");
                    }
                }
            }
        }
    }

    #[test]
    fn guided_filter_flat_guide_has_positive_denominator() {
        let guide = Plane { width: 64, height: 48, data: vec![0.661_649_1; 64 * 48] };
        let mean = guide.box_blur(2);
        let squared = Plane { data: guide.data.iter().map(|v| v * v).collect(), ..guide.clone() }.box_blur(2);
        // This reproduces cancellation to exactly the negative regulariser.
        assert_eq!(squared.data[0] - mean.data[0] * mean.data[0], -f32::EPSILON);
        for eps in [0.0, f32::MIN_POSITIVE, f32::EPSILON] {
            let filtered = guided_filter(&guide, &guide, 2, eps);
            assert!(filtered.data.iter().all(|v| v.is_finite() && (*v - guide.data[0]).abs() < 1e-6));
        }
    }
}

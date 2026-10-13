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

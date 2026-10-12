//! Float RGB image buffer and the resampling / filtering primitives the
//! pipeline shares.

use rayon::prelude::*;

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
        let spans = |from: usize, to: usize| -> Vec<(usize, Vec<f32>)> {
            let scale = from as f64 / to as f64;
            (0..to)
                .map(|i| {
                    let (a, b) = (i as f64 * scale, (i + 1) as f64 * scale);
                    let first = a.floor() as usize;
                    let last = ((b.ceil() as usize).max(first + 1)).min(from);
                    let weights =
                        (first..last).map(|s| ((b.min((s + 1) as f64) - a.max(s as f64)) / scale) as f32).collect();
                    (first, weights)
                })
                .collect()
        };
        let (xs, ys) = (spans(self.width, width), spans(self.height, height));
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
        let rgb =
            Image { width: self.width, height: self.height, data: self.data.iter().map(|&v| [v, 0.0, 0.0]).collect() };
        let out = rgb.resized(width, height);
        Plane { width, height, data: out.data.iter().map(|p| p[0]).collect() }
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
        // Vertical pass on a transposed copy keeps the inner loop contiguous.
        let mut columns = vec![0.0f32; w * h];
        columns.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
            let src: Vec<f32> = (0..h).map(|y| mid[y * w + x]).collect();
            run(&src, col, h, 1);
        });
        let mut out = Plane::new(w, h);
        out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                *v = columns[x * h + y];
            }
        });
        out
    }

    /// Approximate Gaussian blur: three box blurs of matching variance.
    pub fn blur(&self, sigma: f32) -> Plane {
        if sigma < 0.3 {
            return self.clone();
        }
        // Variance of a box of radius r is r(r+1)/3; three passes add up.
        let radius = (((sigma * sigma + 0.25).sqrt() - 0.5).round() as usize).max(1);
        self.box_blur(radius).box_blur(radius).box_blur(radius)
    }
}

/// Edge-preserving smoothing of `input` guided by `guide` (He et al., guided
/// filter). `radius` is the window radius, `eps` the edge threshold in
/// squared guide units.
pub fn guided_filter(guide: &Plane, input: &Plane, radius: usize, eps: f32) -> Plane {
    let mul = |a: &Plane, b: &Plane| Plane {
        width: a.width,
        height: a.height,
        data: a.data.par_iter().zip(&b.data).map(|(x, y)| x * y).collect(),
    };
    let mean_g = guide.box_blur(radius);
    let mean_i = input.box_blur(radius);
    let corr_gi = mul(guide, input).box_blur(radius);
    let corr_gg = mul(guide, guide).box_blur(radius);
    let mut a = Plane::new(guide.width, guide.height);
    let mut b = Plane::new(guide.width, guide.height);
    a.data.par_iter_mut().zip(b.data.par_iter_mut()).enumerate().for_each(|(i, (a, b))| {
        let var = corr_gg.data[i] - mean_g.data[i] * mean_g.data[i];
        let cov = corr_gi.data[i] - mean_g.data[i] * mean_i.data[i];
        *a = cov / (var + eps);
        *b = mean_i.data[i] - *a * mean_g.data[i];
    });
    let (mean_a, mean_b) = (a.box_blur(radius), b.box_blur(radius));
    Plane {
        width: guide.width,
        height: guide.height,
        data: (0..guide.data.len()).into_par_iter().map(|i| mean_a.data[i] * guide.data[i] + mean_b.data[i]).collect(),
    }
}

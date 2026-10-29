//! Local selections in the finished frame, evaluated from one immutable guide.

use crate::color::{self, Spaces};
use crate::image::{Image, Plane};
use crate::params::{LocalMask, MaskShape, Params};
use crate::tone::{self, Filmic};
use rayon::prelude::*;

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A truncated-window mean: replicated border pixels would turn corner noise
/// into a broad selection gradient. f64 sums avoid threshold-visible drift.
fn mask_mean(plane: &Plane, radius: usize) -> Plane {
    let (w, h) = (plane.width, plane.height);
    let mut mid = Plane::new(w, h);
    mid.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let source = &plane.data[y * w..(y + 1) * w];
        let mut sum: f64 = source[..(radius + 1).min(w)].iter().map(|v| *v as f64).sum();
        for (x, v) in row.iter_mut().enumerate() {
            let start = x.saturating_sub(radius);
            let end = (x + radius + 1).min(w);
            *v = (sum / (end - start) as f64) as f32;
            if x >= radius {
                sum -= source[x - radius] as f64;
            }
            if x + radius + 1 < w {
                sum += source[x + radius + 1] as f64;
            }
        }
    });
    // Group adjacent columns so every source cache line serves several sums.
    const TILE: usize = 16;
    let mut columns = vec![0.0; w * h];
    columns.par_chunks_mut(h * TILE).enumerate().for_each(|(tile, out)| {
        let x0 = tile * TILE;
        let n = out.len() / h;
        let mut sums = [0.0f64; TILE];
        for y in 0..(radius + 1).min(h) {
            for (sum, v) in sums[..n].iter_mut().zip(&mid.data[y * w + x0..y * w + x0 + n]) {
                *sum += *v as f64;
            }
        }
        for y in 0..h {
            let start = y.saturating_sub(radius);
            let end = (y + radius + 1).min(h);
            for (v, sum) in out[y * n..(y + 1) * n].iter_mut().zip(&sums[..n]) {
                *v = (*sum / (end - start) as f64) as f32;
            }
            if y >= radius {
                for (sum, v) in sums[..n].iter_mut().zip(&mid.data[(y - radius) * w + x0..(y - radius) * w + x0 + n]) {
                    *sum -= *v as f64;
                }
            }
            if y + radius + 1 < h {
                for (sum, v) in
                    sums[..n].iter_mut().zip(&mid.data[(y + radius + 1) * w + x0..(y + radius + 1) * w + x0 + n])
                {
                    *sum += *v as f64;
                }
            }
        }
    });
    mid.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x0 in (0..w).step_by(TILE) {
            let n = (w - x0).min(TILE);
            let offset = x0 * h + y * n;
            row[x0..x0 + n].copy_from_slice(&columns[offset..offset + n]);
        }
    });
    mid
}

fn refine_lightness(guide: Plane) -> Plane {
    // Broad estimation suppresses both grain and correlated patch noise. The
    // native guide still reconstructs edges; a small-support detail gate keeps
    // thin occluders that a broad regression could otherwise attenuate.
    let mean = mask_mean(&guide, 128);
    let mut squared =
        Plane { width: guide.width, height: guide.height, data: guide.data.par_iter().map(|v| v * v).collect() };
    squared = mask_mean(&squared, 128);
    let mut a = squared;
    let mut b = mean;
    a.data.par_iter_mut().zip(b.data.par_iter_mut()).for_each(|(a, b)| {
        let var = (*a - *b * *b).max(0.0);
        *a = var / (var + 0.0064);
        *b *= 1.0 - *a;
    });
    a = mask_mean(&a, 128);
    b = mask_mean(&b, 128);
    let detail = mask_mean(&guide, 2);
    a.data.par_iter_mut().enumerate().for_each(|(i, out)| {
        let edge = smooth(((guide.data[i] - detail.data[i]).abs() - 0.04) / 0.06);
        let refined = *out * guide.data[i] + b.data[i];
        *out = (refined + edge * (guide.data[i] - refined)).clamp(0.0, 1.0);
    });
    a
}

/// Extreme overlapping entries must not report success with NaN/black exports.
/// These safety bounds are far above useful photographic light levels; a
/// common gain preserves signed channels and chromaticity at the ceiling.
fn local_gain(pixel: [f32; 3], stops: f32) -> [f32; 3] {
    if stops == 0.0 {
        return pixel;
    }
    let mut gain = stops.clamp(-32.0, 32.0).exp2();
    let magnitude = pixel.iter().copied().map(f32::abs).fold(0.0, f32::max);
    if magnitude > 0.0 {
        gain = gain.min(65536.0 / magnitude);
    }
    pixel.map(|v| v * gain)
}

struct Geometry {
    origin: [f32; 2],
    axis: [f32; 2],
    radii: [f32; 2],
    rotation: [f32; 2],
    radial: bool,
    all: bool,
    feather: f32,
}

impl Geometry {
    fn new(entry: &LocalMask, width: usize, height: usize) -> Self {
        let (w, h) = ((width.max(2) - 1) as f32, (height.max(2) - 1) as f32);
        let radial = entry.shape == MaskShape::Radial;
        let origin = entry.center.or(entry.from).unwrap_or([0.0, 0.0]);
        let origin = [origin[0] as f32 * w, origin[1] as f32 * h];
        let to = entry.to.unwrap_or([0.0, 0.0]);
        let axis = [to[0] as f32 * w - origin[0], to[1] as f32 * h - origin[1]];
        let scale = 1.0 / (axis[0] * axis[0] + axis[1] * axis[1]).max(1e-6);
        let radii = entry.radii.unwrap_or([1.0, 1.0]);
        let (sin, cos) = (entry.rotation as f32).to_radians().sin_cos();
        Self {
            origin,
            axis: axis.map(|v| v * scale),
            radii: [radii[0] as f32 * w, radii[1] as f32 * h],
            rotation: [sin, cos],
            radial,
            all: !radial && entry.from.is_none(),
            feather: entry.feather as f32,
        }
    }

    fn weight(&self, x: usize, y: usize) -> f32 {
        if self.all {
            return 1.0;
        }
        let (x, y) = (x as f32 - self.origin[0], y as f32 - self.origin[1]);
        if self.radial {
            let [sin, cos] = self.rotation;
            // Rotate in pixel space; frame fractions alone distort rotated ellipses.
            let r = ((x * cos + y * sin) / self.radii[0]).hypot((-x * sin + y * cos) / self.radii[1]);
            1.0 - smooth((r - (1.0 - self.feather)) / self.feather)
        } else {
            1.0 - smooth(x * self.axis[0] + y * self.axis[1])
        }
    }
}

/// Below this Oklab chroma a pixel has no dependable hue; selection reaches
/// full strength one ramp width above it.
const COLOUR_FLOOR: f32 = 0.015;
const COLOUR_RAMP: f32 = 0.03;
/// Joint guided filter: hue is noisy at native resolution, but the edges of
/// a coloured object are also edges in lightness, so the guide keeps them.
const COLOUR_RADIUS: usize = 6;
const COLOUR_EPS: f32 = 0.0004;

/// Raw membership of each pixel in a set of HSL bands, from the same band
/// weights as `hsl.*`, so bands that neighbour each other select their hues
/// in between completely.
fn colour_membership(lab: &[[f32; 3]], width: usize, height: usize, bands: &[bool; 8]) -> Plane {
    Plane {
        width,
        height,
        data: lab
            .par_iter()
            .map(|&[_, a, b]| {
                let (i, j, t) = crate::adjust::band_weights(b.atan2(a).to_degrees());
                let hue = bands[i] as u8 as f32 * (1.0 - t) + bands[j] as u8 as f32 * t;
                hue * chroma_eligibility((a * a + b * b).sqrt())
            })
            .collect(),
    }
}

fn chroma_eligibility(chroma: f32) -> f32 {
    smooth((chroma - COLOUR_FLOOR) / COLOUR_RAMP)
}

fn guided(guide: &Plane, input: &Plane, radius: usize, eps: f32) -> Plane {
    let product = |a: &Plane, b: &Plane| Plane {
        width: a.width,
        height: a.height,
        data: a.data.par_iter().zip(&b.data).map(|(a, b)| a * b).collect(),
    };
    let mean_i = mask_mean(guide, radius);
    let mean_p = mask_mean(input, radius);
    let corr_ip = mask_mean(&product(guide, input), radius);
    let corr_ii = mask_mean(&product(guide, guide), radius);
    let mut a = corr_ii;
    let mut b = mean_p;
    a.data.par_iter_mut().zip(b.data.par_iter_mut()).enumerate().for_each(|(k, (a, b))| {
        let mi = mean_i.data[k];
        let var = (*a - mi * mi).max(0.0);
        let cov = corr_ip.data[k] - mi * *b;
        *a = cov / (var + eps);
        *b -= *a * mi;
    });
    let a = mask_mean(&a, radius);
    let mut b = mask_mean(&b, radius);
    b.data.par_iter_mut().enumerate().for_each(|(k, out)| {
        *out = (a.data[k] * guide.data[k] + *out).clamp(0.0, 1.0);
    });
    b
}

pub(crate) struct Masks<'a> {
    entries: &'a [LocalMask],
    geometry: Vec<Geometry>,
    lightness: Option<Plane>,
    /// Refined colour selections, one per distinct set of bands.
    colour_planes: Vec<Plane>,
    /// Index into `colour_planes` for each entry that names colours.
    colours: Vec<Option<usize>>,
}

impl<'a> Masks<'a> {
    pub(crate) fn new(image: &Image, params: &'a Params, spaces: &Spaces) -> Self {
        let filmic = Filmic::new(params.contrast as f32, crate::pipeline::BASE_EXPOSURE.exp2());
        let lightness = params.masks.iter().any(|m| m.luma.is_some()).then(|| {
            let guide = Plane::from_fn(image, |p| spaces.to_oklab(filmic.map_rgb(p))[0].clamp(0.0, 1.0));
            refine_lightness(guide)
        });
        let mut colours = vec![None; params.masks.len()];
        let mut colour_planes = Vec::new();
        if params.masks.iter().any(|m| !m.colors.is_empty()) {
            let lab: Vec<[f32; 3]> = image.data.par_iter().map(|&p| spaces.to_oklab(filmic.map_rgb(p))).collect();
            // The guided filter follows lightness, so next to grey of the same
            // lightness it would spread colour into the grey. Chroma, unlike
            // hue, is steady on near-neutral noise: a lightly smoothed
            // eligibility bounds the refined selection to colourful pixels.
            let chroma = Plane {
                width: image.width,
                height: image.height,
                data: lab.par_iter().map(|p| (p[1] * p[1] + p[2] * p[2]).sqrt()).collect(),
            };
            // The pixel's own chroma must qualify as well, so a thin grey line
            // among selected colour is not carried in by its neighbours.
            let mut eligible = mask_mean(&chroma, 1);
            eligible.data.par_iter_mut().zip(&chroma.data).for_each(|(c, own)| {
                *c = chroma_eligibility(*c).min(chroma_eligibility(*own));
            });
            let guide = Plane {
                width: image.width,
                height: image.height,
                data: lab.par_iter().map(|p| p[0].clamp(0.0, 1.0)).collect(),
            };
            let mut done: Vec<[bool; 8]> = Vec::new();
            for (n, entry) in params.masks.iter().enumerate().filter(|(_, m)| !m.colors.is_empty()) {
                let bands = crate::params::HSL_BANDS.map(|band| entry.colors.iter().any(|c| c == band));
                // Entries that name the same colours share one selection.
                if let Some(index) = done.iter().position(|b| *b == bands) {
                    colours[n] = Some(index);
                    continue;
                }
                let membership = colour_membership(&lab, image.width, image.height, &bands);
                let mut plane = guided(&guide, &membership, COLOUR_RADIUS, COLOUR_EPS);
                plane.data.par_iter_mut().zip(&eligible.data).for_each(|(v, e)| *v *= e);
                colours[n] = Some(colour_planes.len());
                colour_planes.push(plane);
                done.push(bands);
            }
        }
        Self {
            entries: &params.masks,
            geometry: params.masks.iter().map(|m| Geometry::new(m, image.width, image.height)).collect(),
            lightness,
            colour_planes,
            colours,
        }
    }

    pub(crate) fn weight(&self, n: usize, x: usize, y: usize) -> f32 {
        let entry = &self.entries[n];
        let mut shape = self.geometry[n].weight(x, y);
        let mut luma = 1.0;
        if let (Some([low, high]), Some(guide)) = (entry.luma, &self.lightness) {
            let v = guide.data[y * guide.width + x].clamp(0.0, 1.0);
            let feather = entry.luma_feather as f32;
            let lower = if low == 0.0 { 1.0 } else { smooth((v - low as f32 + feather) / feather) };
            let upper = if high == 1.0 { 1.0 } else { smooth((high as f32 + feather - v) / feather) };
            luma = lower * upper;
        }
        if let Some(index) = self.colours[n] {
            let colour = &self.colour_planes[index];
            luma *= colour.data[y * colour.width + x];
        }
        if entry.invert {
            if self.geometry[n].all {
                luma = 1.0 - luma;
            } else {
                shape = 1.0 - shape;
            }
        }
        (shape * luma).clamp(0.0, 1.0)
    }

    pub(crate) fn view(&self, n: usize, width: usize, height: usize) -> Image {
        let mut image = Image::new(width, height);
        image.map_xy(|x, y, _| [self.weight(n, x, y); 3]);
        image
    }

    pub(crate) fn apply_scene(&self, image: &mut Image) {
        if self.entries.iter().any(|m| m.exposure != 0.0) {
            image.map_xy(|x, y, p| {
                let stops: f32 =
                    self.entries.iter().enumerate().map(|(n, m)| m.exposure as f32 * self.weight(n, x, y)).sum();
                local_gain(p, stops)
            });
        }
        if self.entries.iter().any(|m| m.shadows != 0.0 || m.highlights != 0.0) {
            let log = Plane::from_fn(image, |p| color::luma(p).max(1e-6).log2());
            let base = tone::base_layer(&log);
            let pivot = tone::GREY.log2();
            image.map_xy(|x, y, p| {
                let d = base.data[y * base.width + x] - pivot;
                let stops: f32 = self
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(n, m)| {
                        let value = m.highlights as f32 / 100.0 * tone::ramp(d, 0.7)
                            + m.shadows as f32 / 100.0 * tone::ramp(-d, 0.7);
                        value * self.weight(n, x, y)
                    })
                    .sum();
                local_gain(p, stops)
            });
        }
    }

    pub(crate) fn apply_saturation(&self, image: &mut Image, spaces: &Spaces) {
        if !self.entries.iter().any(|m| m.saturation != 0.0) {
            return;
        }
        image.map_xy(|x, y, p| {
            let gain: f32 = self
                .entries
                .iter()
                .enumerate()
                .map(|(n, m)| (1.0 + m.saturation as f32 / 100.0 * self.weight(n, x, y)).max(0.0))
                .product();
            if gain == 1.0 {
                return p;
            }
            crate::adjust::ColorOps::saturate(p, gain - 1.0, spaces)
        });
    }

    pub(crate) fn contrast_curves(&self, params: &Params) -> Vec<(usize, Filmic)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, m)| m.contrast != 0.0)
            .map(|(n, m)| {
                (
                    n,
                    Filmic::new(
                        (params.contrast + m.contrast).clamp(-100.0, 100.0) as f32,
                        crate::pipeline::BASE_EXPOSURE.exp2(),
                    ),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Crop;
    use crate::pipeline::{self, Size};
    use crate::synthetic::Synthetic;

    fn params(text: &str) -> Params {
        Params::from_json(text).unwrap()
    }

    #[test]
    fn truncated_mask_means_match_independent_window_sums() {
        for (w, h) in [(1, 1), (1, 7), (7, 1), (13, 9), (37, 3)] {
            let p = Plane { width: w, height: h, data: (0..w * h).map(|i| (i % 17) as f32 / 17.0).collect() };
            for r in [1, 4, 128] {
                let mean = mask_mean(&p, r);
                for y in 0..h {
                    for x in 0..w {
                        let mut sum = 0.0f64;
                        let mut n = 0;
                        for yy in y.saturating_sub(r)..=(y + r).min(h - 1) {
                            for xx in x.saturating_sub(r)..=(x + r).min(w - 1) {
                                sum += p.data[yy * w + xx] as f64;
                                n += 1;
                            }
                        }
                        assert!((mean.data[y * w + x] - (sum / n as f64) as f32).abs() < 1e-6);
                    }
                }
            }
        }
    }

    #[test]
    fn radial_geometry_rotation_feather_invert_and_product() {
        let p = params(r#"{"masks":[{"shape":"radial","center":[0.5,0.5],"radii":[0.3,0.2],"feather":0.5}]}"#);
        let image = Image { width: 201, height: 101, data: vec![[0.2; 3]; 20301] };
        let masks = Masks::new(&image, &p, &Spaces::new());
        assert_eq!(masks.weight(0, 100, 50), 1.0);
        assert!((masks.weight(0, 145, 50) - 0.5).abs() < 1e-6);
        assert_eq!(masks.weight(0, 160, 50), 0.0);
        assert_eq!(masks.weight(0, 100, 70), 0.0);
        let mut p = p.clone();
        p.masks[0].rotation = 90.0;
        let masks = Masks::new(&image, &p, &Spaces::new());
        assert!((masks.weight(0, 100, 95) - 0.5).abs() < 1e-6);
        assert_eq!(masks.weight(0, 120, 50), 0.0);
        p.masks[0].invert = true;
        let inverse = Masks::new(&image, &p, &Spaces::new());
        assert_eq!(inverse.weight(0, 100, 50), 0.0);
        assert_eq!(inverse.weight(0, 140, 50), 1.0);
        p.masks[0].luma = Some([0.95, 1.0]);
        let product = Masks::new(&image, &p, &Spaces::new());
        assert_eq!(product.weight(0, 140, 50), 0.0);
    }

    #[test]
    fn colour_selection_follows_hue_and_lightness_edges() {
        // Yellow leaves, blue sky and a grey stone side by side.
        let (w, h) = (240, 60);
        let colour = |x: usize| match x {
            0..80 => [0.40, 0.33, 0.04],
            80..160 => [0.05, 0.10, 0.40],
            _ => [0.2; 3],
        };
        let image = Image { width: w, height: h, data: (0..w * h).map(|i| colour(i % w)).collect() };
        let p = params(r#"{"masks":[{"colors":["yellow","orange"],"exposure":1}]}"#);
        let masks = Masks::new(&image, &p, &Spaces::new());
        assert!(masks.weight(0, 40, 30) > 0.95, "{}", masks.weight(0, 40, 30));
        assert!(masks.weight(0, 120, 30) < 0.02);
        assert!(masks.weight(0, 200, 30) < 0.02);
        // The edge follows lightness: it is complete within the filter radius.
        assert!(masks.weight(0, 78, 30) > 0.9 && masks.weight(0, 82, 30) < 0.1);
        let p = params(r#"{"masks":[{"colors":["blue"],"invert":true,"exposure":1}]}"#);
        let masks = Masks::new(&image, &p, &Spaces::new());
        assert!(masks.weight(0, 120, 30) < 0.02 && masks.weight(0, 40, 30) > 0.98 && masks.weight(0, 200, 30) > 0.98);
        // Entries with the same colours share the selection; shape multiplies.
        let p = params(
            r#"{"masks":[{"colors":["yellow"],"exposure":1},{"shape":"radial","center":[0.1,0.5],"radii":[0.05,0.4],"colors":["yellow"],"exposure":1}]}"#,
        );
        let masks = Masks::new(&image, &p, &Spaces::new());
        assert_eq!((masks.colours[0], masks.colours[1], masks.colour_planes.len()), (Some(0), Some(0), 1));
        assert!(masks.weight(1, 24, 30) > 0.9 && masks.weight(1, 60, 30) == 0.0);
    }

    #[test]
    fn colour_selection_does_not_leak_into_grey_of_the_same_lightness() {
        let spaces = Spaces::new();
        let filmic = Filmic::new(0.0, crate::pipeline::BASE_EXPOSURE.exp2());
        let yellow = [0.40, 0.33, 0.04];
        let target = spaces.to_oklab(filmic.map_rgb(yellow))[0];
        // Find the scene grey whose display lightness matches the yellow.
        let (mut lo, mut hi) = (0.0f32, 2.0f32);
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            if spaces.to_oklab(filmic.map_rgb([mid; 3]))[0] < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (w, h) = (120, 40);
        let image = Image {
            width: w,
            height: h,
            data: (0..w * h).map(|i| if i % w < 60 { yellow } else { [lo; 3] }).collect(),
        };
        let p = params(r#"{"masks":[{"colors":["yellow","orange"],"exposure":1}]}"#);
        let masks = Masks::new(&image, &p, &Spaces::new());
        assert!(masks.weight(0, 30, 20) > 0.95);
        for x in 62..w {
            assert!(masks.weight(0, x, 20) < 0.01, "grey at {x}: {}", masks.weight(0, x, 20));
        }
        // A one-pixel grey line inside the colour stays out as well.
        let mut image = Image { width: w, height: h, data: vec![yellow; w * h] };
        for y in 0..h {
            image.data[y * w + 60] = [lo; 3];
        }
        let masks = Masks::new(&image, &p, &Spaces::new());
        assert!(masks.weight(0, 60, 20) < 0.01, "{}", masks.weight(0, 60, 20));
        assert!(masks.weight(0, 58, 20) > 0.9);
    }

    #[test]
    fn colour_selection_is_quiet_on_noisy_near_neutral_areas() {
        // Chroma noise on grey has random hue; it must not become a speckled selection.
        let (w, h) = (160, 160);
        let mut seed = 7u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let data = (0..w * h).map(|_| [0.2 + 0.01 * noise(), 0.2 + 0.01 * noise(), 0.2 + 0.01 * noise()]).collect();
        let image = Image { width: w, height: h, data };
        let p = params(r#"{"masks":[{"colors":["yellow"],"exposure":1}]}"#);
        let masks = Masks::new(&image, &p, &Spaces::new());
        let max =
            (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| masks.weight(0, x, y)).fold(0.0, f32::max);
        assert!(max < 0.05, "{max}");
    }

    #[test]
    fn mask_positions_follow_finished_rotation_and_crop() {
        let mut synthetic = Synthetic::from_camera_rgb(202, 102, |_, _| [0.1; 3]);
        synthetic.orientation = 6;
        let raw = crate::arw::decode(&synthetic.to_bytes()).unwrap();
        let mut p = params(r#"{"masks":[{"shape":"radial","center":[0.5,0.5],"radii":[0.25,0.25]}]}"#);
        p.crop = Crop { left: 0.2, top: 0.25, right: 0.8, bottom: 0.75, ..Crop::default() };
        p.rotate = 7.0;
        let scene = pipeline::scene(&raw, &p, Size::default()).unwrap().image;
        let view = pipeline::render_mask(&raw, &p, Size::default(), 0).unwrap().image;
        assert_eq!((view.width, view.height), (scene.width, scene.height));
        assert_eq!(view.at(view.width / 2, view.height / 2), [1.0; 3]);
        assert_eq!(view.at(0, 0), [0.0; 3]);
    }

    #[test]
    fn luma_separates_thin_occluders_and_has_bounded_edge_overshoot() {
        let p = params(r#"{"masks":[{"luma":[0.75,1],"exposure":-1}]}"#);
        let filmic = Filmic::new(0.0, pipeline::BASE_EXPOSURE.exp2());
        let image = Image {
            width: 160,
            height: 64,
            data: (0..160 * 64)
                .map(|i| {
                    let x = i % 160;
                    [if x < 80 || x == 95 || x == 121 { 0.02 } else { 1.0 }; 3]
                })
                .collect(),
        };
        let masks = Masks::new(&image, &p, &Spaces::new());
        for y in 0..64 {
            for x in 0..160 {
                let m = masks.weight(0, x, y);
                if x < 80 || x == 95 || x == 121 {
                    assert!(m <= 0.001, "dark {x}: {m}");
                } else {
                    assert!(m >= 0.99, "sky {x}: {m}");
                }
            }
        }
        let mut edited = image.clone();
        masks.apply_scene(&mut edited);
        for (a, b) in image.data.iter().zip(&edited.data) {
            let before = filmic.map_rgb(*a)[0];
            let after = filmic.map_rgb(*b)[0];
            if a[0] < 0.1 {
                assert!((after - before).abs() < 1e-6);
            } else {
                assert!(after <= before && after >= filmic.map_rgb([0.5; 3])[0] - 1e-5);
            }
        }
        // A bright one-pixel line against dark surroundings is also retained.
        let image = Image {
            width: 160,
            height: 64,
            data: (0..160 * 64).map(|i| [if i % 160 == 80 { 1.0 } else { 0.02 }; 3]).collect(),
        };
        let masks = Masks::new(&image, &p, &Spaces::new());
        assert!(masks.weight(0, 80, 32) > 0.99);
        assert_eq!(masks.weight(0, 79, 32), 0.0);
    }

    #[test]
    fn finished_luma_edge_matches_ideal_selection_without_halos() {
        let spaces = Spaces::new();
        let image = Image {
            width: 192,
            height: 96,
            data: (0..192 * 96).map(|i| [if i % 192 < 96 || i % 192 == 121 { 0.02 } else { 0.65 }; 3]).collect(),
        };
        let p = params(r#"{"masks":[{"luma":[0.75,1],"exposure":-1}]}"#);
        let mut ideal = image.clone();
        ideal.map_xy(|x, _, p| if x >= 96 && x != 121 { p.map(|v| v * 0.5) } else { p });
        let expected = pipeline::finish(&ideal, &Params::default(), &spaces);
        let actual = pipeline::finish(&image, &p, &spaces);
        for (i, (p, q)) in actual.data.iter().zip(expected.data).enumerate() {
            for c in 0..3 {
                assert!((p[c] - q[c]).abs() < 1e-5, "edge {}: {p:?} vs {q:?}", i % 192);
            }
        }
    }

    fn variance(image: &Image) -> f64 {
        let mean = image.data.iter().map(|p| p[0] as f64).sum::<f64>() / image.data.len() as f64;
        image.data.iter().map(|p| (p[0] as f64 - mean).powi(2)).sum::<f64>() / image.data.len() as f64
    }

    #[test]
    fn feathered_luma_does_not_amplify_flat_patch_noise() {
        // Both boundaries, narrowest feather, maximum darkening and several noise amplitudes.
        let spaces = Spaces::new();
        for range in [[0.5, 1.0], [0.0, 0.5]] {
            let p = Params::from_value(serde_json::json!({"masks":[{"luma":range,"luma_feather":0.02,"exposure":-3}]}))
                .unwrap();
            for amplitude in [0.0002, 0.002, 0.01] {
                let filmic = Filmic::new(0.0, pipeline::BASE_EXPOSURE.exp2());
                let target = if range[0] > 0.0 { 0.49 } else { 0.51 };
                let (mut lo, mut hi) = (0.0, 1.0);
                for _ in 0..40 {
                    let mid = (lo + hi) / 2.0;
                    if spaces.to_oklab(filmic.map_rgb([mid; 3]))[0] < target {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                let value = (lo + hi) / 2.0;
                for patch_size in [1usize, 4, 8] {
                    let image = Image {
                        width: 128,
                        height: 96,
                        data: (0usize..128 * 96)
                            .map(|i| {
                                let tile = (i % 128 / patch_size) + (i / 128 / patch_size) * (128 / patch_size);
                                let hash = (tile as u32).wrapping_mul(747796405).wrapping_add(2891336453);
                                let noise = (hash ^ (hash >> 16)) as f32 / u32::MAX as f32 - 0.5;
                                [value + noise * amplitude; 3]
                            })
                            .collect(),
                    };
                    let masks = Masks::new(&image, &p, &spaces);
                    let mut edited = image.clone();
                    masks.apply_scene(&mut edited);
                    assert!(
                        variance(&edited) <= variance(&image) * 1.01,
                        "{range:?} {amplitude}: {} -> {}",
                        variance(&image),
                        variance(&edited)
                    );
                    let weights: Vec<_> = (0..128 * 96).map(|i| masks.weight(0, i % 128, i / 128)).collect();
                    let mean = weights.iter().sum::<f32>() / weights.len() as f32;
                    let rms = (weights.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / weights.len() as f32).sqrt();
                    assert!(rms < 0.02, "noisy mask RMS {rms} {range:?} amp {amplitude} patch {patch_size}");
                }
            }
        }
    }

    #[test]
    fn maximum_overlapping_local_and_global_adjustments_remain_finite() {
        let spaces = Spaces::new();
        let image = Image {
            width: 96,
            height: 64,
            data: (0..96 * 64)
                .map(|i| {
                    let v = if i % 96 < 32 {
                        1e-5
                    } else if i % 96 < 64 {
                        0.15
                    } else {
                        1.2
                    };
                    [v * 0.7, v, v * 0.8]
                })
                .collect(),
        };
        for exposure in [-3.0, 3.0] {
            for local_tone in [-100.0, 100.0] {
                let entry = serde_json::json!({"luma":[0,1],"exposure":exposure,"shadows":local_tone,"highlights":local_tone,"contrast":100,"saturation":100});
                let p = Params::from_value(serde_json::json!({
                    "exposure":5,"highlights":100,"shadows":100,"dehaze":100,"vignette":100,
                    "gradients":vec![serde_json::json!({"from":[0,0],"to":[1,1],"exposure":3,"saturation":100});4],
                    "masks":vec![entry;8]
                }))
                .unwrap();
                let mut prepared = image.clone();
                prepared.map(|p| p.map(|v| v * 2.0f32.powi(18)));
                let masks = Masks::new(&prepared, &p, &spaces);
                masks.apply_scene(&mut prepared);
                assert!(prepared.data.iter().flatten().all(|v| v.is_finite() && v.abs() <= 65536.0));
                let actual = pipeline::finish(&image, &p, &spaces);
                assert!(
                    actual.data.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                    "{exposure} {local_tone}"
                );
                if local_tone > 0.0 {
                    assert!(
                        actual.data.iter().flatten().any(|v| *v > 0.01),
                        "positive extreme adjustments must not become a NaN-black export"
                    );
                }
            }
        }
    }

    #[test]
    fn noisy_flat_patch_in_nonflat_frame_is_not_amplified() {
        let spaces = Spaces::new();
        let filmic = Filmic::new(0.0, pipeline::BASE_EXPOSURE.exp2());
        for range in [[0.5, 1.0], [0.0, 0.5]] {
            let target = if range[0] > 0.0 { 0.49 } else { 0.51 };
            let (mut lo, mut hi) = (0.0, 1.0);
            for _ in 0..40 {
                let m = (lo + hi) / 2.0;
                if spaces.to_oklab(filmic.map_rgb([m; 3]))[0] < target {
                    lo = m;
                } else {
                    hi = m;
                }
            }
            let value = (lo + hi) / 2.0;
            let image = Image {
                width: 1024,
                height: 384,
                data: (0usize..1024 * 384)
                    .map(|i| {
                        let tile = (i % 1024 / 8) + (i / 1024 / 8) * 128;
                        let hash = (tile as u32).wrapping_mul(747796405).wrapping_add(2891336453);
                        let noise = (hash ^ (hash >> 16)) as f32 / u32::MAX as f32 - 0.5;
                        [if i % 1024 < 32 { 0.01 } else { value + noise * 0.01 }; 3]
                    })
                    .collect(),
            };
            let p = Params::from_value(serde_json::json!({"masks":[{"luma":range,"luma_feather":0.02,"exposure":-3}]}))
                .unwrap();
            let masks = Masks::new(&image, &p, &spaces);
            let mut edited = image.clone();
            masks.apply_scene(&mut edited);
            let before = image.crop(384, 128, 256, 128);
            let after = edited.crop(384, 128, 256, 128);
            assert!(
                variance(&after) <= variance(&before) * 1.01,
                "{range:?}: {} -> {}",
                variance(&before),
                variance(&after)
            );
        }
    }

    #[test]
    fn local_masks_ignore_rayon_scheduling_and_other_entry_adjustments() {
        let image = Image {
            width: 128,
            height: 96,
            data: (0..128 * 96)
                .map(|i| {
                    let v = 0.01 + (i % 128) as f32 / 128.0;
                    [v * 0.7, v, v * 0.8]
                })
                .collect(),
        };
        let p = params(
            r#"{"masks":[{"shape":"radial","center":[0.6,0.5],"radii":[0.4,0.3],"rotation":30,"luma":[0.4,1],"exposure":0.7,"shadows":25,"contrast":20,"saturation":15},{"luma":[0.75,1],"exposure":-0.7,"highlights":-25}]}"#,
        );
        let spaces = Spaces::new();
        let expected = pipeline::finish(&image, &p, &spaces);
        for threads in [1, 2, 4] {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            let actual = pool.install(|| pipeline::finish(&image, &p, &spaces));
            assert_eq!(actual.data, expected.data);
            assert_eq!(
                crate::output::encode(std::path::Path::new("mask.png"), &actual, 92).unwrap(),
                crate::output::encode(std::path::Path::new("mask.png"), &expected, 92).unwrap()
            );
        }
        let mut changed = p.clone();
        changed.masks[0].exposure = -2.0;
        changed.masks[0].contrast = -70.0;
        let original = Masks::new(&image, &p, &spaces);
        let changed = Masks::new(&image, &changed, &spaces);
        assert_eq!(original.view(1, 128, 96).data, changed.view(1, 128, 96).data);
    }

    #[test]
    fn full_selection_uses_global_adjustment_operators() {
        let spaces = Spaces::new();
        let image = Image {
            width: 96,
            height: 64,
            data: (0..96 * 64)
                .map(|i| {
                    let v = 0.008 + (i % 96) as f32 / 95.0 * 0.8;
                    [v * 0.8, v, v * 0.7]
                })
                .collect(),
        };
        for field in ["exposure", "shadows", "highlights", "contrast", "saturation"] {
            let v = if field == "exposure" { 0.5 } else { 35.0 };
            let local = Params::from_value(serde_json::json!({"sharpen":0,"masks":[{"luma":[0,1],field:v}]})).unwrap();
            let global = Params::from_value(serde_json::json!({"sharpen":0,field:v})).unwrap();
            let a = pipeline::finish(&image, &local, &spaces);
            let b = pipeline::finish(&image, &global, &spaces);
            let max =
                a.data.iter().zip(b.data).flat_map(|(p, q)| [0, 1, 2].map(|c| (p[c] - q[c]).abs())).fold(0.0, f32::max);
            assert!(max < 2e-5, "{field}: {max}");
        }
    }
}

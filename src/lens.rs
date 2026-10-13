//! Lens corrections from the tables Sony cameras embed in the RAW file:
//! vignetting, geometric distortion and lateral chromatic aberration. All
//! three are radial functions of the distance from the image centre.

use rayon::prelude::*;

use crate::arw::LensTables;
use crate::image::Image;

/// Piecewise-linear radial function; knot `i` of `n` lies at
/// `(i + 0.5) / (n - 1)` of the half diagonal.
struct Radial(Vec<f32>);

impl Radial {
    #[inline]
    fn at(&self, r: f32) -> f32 {
        let n = self.0.len();
        let pos = (r * (n - 1) as f32 - 0.5).clamp(0.0, (n - 1) as f32);
        let i = (pos as usize).min(n - 2);
        self.0[i] + (self.0[i + 1] - self.0[i]) * (pos - i as f32)
    }
}

/// Catmull-Rom sample of one channel at continuous coordinates.
#[inline]
fn cubic(image: &Image, channel: usize, x: f32, y: f32) -> f32 {
    let (w, h) = (image.width as isize, image.height as isize);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let weights = |t: f32| {
        let (t2, t3) = (t * t, t * t * t);
        [-0.5 * t3 + t2 - 0.5 * t, 1.5 * t3 - 2.5 * t2 + 1.0, -1.5 * t3 + 2.0 * t2 + 0.5 * t, 0.5 * t3 - 0.5 * t2]
    };
    let (wx, wy) = (weights(fx), weights(fy));
    let mut sum = 0.0;
    for (j, wy) in wy.iter().enumerate() {
        let yy = (y0 as isize + j as isize - 1).clamp(0, h - 1) as usize;
        let row = &image.data[yy * image.width..(yy + 1) * image.width];
        let mut acc = 0.0;
        for (i, wx) in wx.iter().enumerate() {
            let xx = (x0 as isize + i as isize - 1).clamp(0, w - 1) as usize;
            acc += row[xx][channel] * wx;
        }
        sum += acc * wy;
    }
    sum
}

/// Applies the embedded corrections to a white-balanced camera-RGB image.
/// Returns `None` when the tables describe an impossible lens (damaged
/// file); the caller then leaves the picture uncorrected.
pub fn correct(image: &Image, tables: &LensTables) -> Option<Image> {
    let (w, h) = (image.width, image.height);
    let (cx, cy) = ((w as f32 - 1.0) * 0.5, (h as f32 - 1.0) * 0.5);
    let half_diagonal = (cx * cx + cy * cy).sqrt();

    let n = tables.distortion.len();
    if w < 2
        || h < 2
        || n < 2
        || [tables.ca_red.len(), tables.ca_blue.len(), tables.vignetting.len()].iter().any(|&len| len != n)
    {
        return None;
    }
    let distortion = Radial(tables.distortion.iter().map(|&v| 1.0 + v as f32 / 16384.0).collect());
    let ca = [&tables.ca_red, &tables.ca_blue].map(|t| Radial(t.iter().map(|&v| 1.0 + v as f32 / 2097152.0).collect()));
    if !distortion.0.iter().all(|v| (0.25..=4.0).contains(v)) {
        return None;
    }
    // Inverse mapping, following Sony's embedded-table convention: positive
    // CA samples farther from the centre. Interpolate distortion and CA
    // separately; interpolation does not commute with their multiplication.
    // Fill zoom is applied to coordinates BEFORE looking up their radius.
    let edge_radius = cx.min(cy) / half_diagonal;
    let max_edge_scale = |fill: f32| {
        let (start, end) = (edge_radius / fill, 1.0 / fill);
        // A linear spline's maximum occurs at an endpoint or a knot; check
        // those exactly, including nonmonotonic distortion tables.
        let mut largest = distortion.at(start).max(distortion.at(end));
        for (i, &value) in distortion.0.iter().enumerate() {
            let knot = (i as f32 + 0.5) / (n - 1) as f32;
            if (start..=end).contains(&knot) {
                largest = largest.max(value);
            }
        }
        largest / fill
    };
    let (mut lo, mut hi) = (0.25, 4.0);
    for _ in 0..32 {
        let mid = (lo + hi) * 0.5;
        if max_edge_scale(mid) > 1.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let fill = hi;
    // Light falloff the lens caused, as a gain that undoes it.
    let gain = Radial(tables.vignetting.iter().map(|&v| ((v as f32 / 8192.0 - 1.0).exp2() - 0.5).exp2()).collect());

    let mut out = Image::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let dy = y as f32 - cy;
        for (x, p) in row.iter_mut().enumerate() {
            let dx = x as f32 - cx;
            let r = (dx * dx + dy * dy).sqrt() / half_diagonal / fill;
            let d = distortion.at(r) / fill;
            for c in 0..3 {
                let s = d * match c {
                    0 => ca[0].at(r),
                    2 => ca[1].at(r),
                    _ => 1.0,
                };
                let (sx, sy) = (cx + dx * s, cy + dy * s);
                // Vignetting is a property of where the light landed on the sensor.
                let source_r = r * fill * s;
                p[c] = cubic(image, c, sx, sy).max(0.0) * gain.at(source_r);
            }
        }
    });
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoomed_radius_and_independent_tables_match_the_inverse_mapping() {
        let (w, h) = (401, 301);
        let mut image = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                image.data[y * w + x] = [x as f32 / w as f32; 3];
            }
        }
        let tables = LensTables {
            distortion: vec![0, 1600, 3200, 4800],
            ca_red: vec![0, 8000, 16000, 24000],
            ca_blue: vec![0, -4000, -8000, -12000],
            vignetting: vec![0; 4],
        };
        let out = correct(&image, &tables).unwrap();
        // Increasing distortion: the corner sets fill. Its unzoomed radius
        // is 1/fill and its source radius is exactly one.
        let (mut lo, mut hi) = (1.0f32, 2.0f32);
        let radial = |values: &[i16], r: f32, divisor: f32| {
            let p = (r * 3.0 - 0.5).clamp(0.0, 3.0);
            let i = (p as usize).min(2);
            1.0 + (values[i] as f32 + (values[i + 1] - values[i]) as f32 * (p - i as f32)) / divisor
        };
        for _ in 0..32 {
            let mid = (lo + hi) * 0.5;
            if radial(&tables.distortion, 1.0 / mid, 16384.0) > mid {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let fill = hi;
        for (x, y) in [(340, 120), (290, 240), (80, 70)] {
            let (dx, dy) = (x as f32 - 200.0, y as f32 - 150.0);
            let r = (dx * dx + dy * dy).sqrt() / 250.0 / fill;
            for c in 0..3 {
                let ca = match c {
                    0 => radial(&tables.ca_red, r, 2097152.0),
                    2 => radial(&tables.ca_blue, r, 2097152.0),
                    _ => 1.0,
                };
                let sx = 200.0 + dx * radial(&tables.distortion, r, 16384.0) * ca / fill;
                assert!((out.at(x, y)[c] - sx / w as f32).abs() < 2e-6, "channel {c}, ({x},{y})");
            }
        }
    }

    #[test]
    fn fill_checks_interior_knots_and_malformed_tables_are_refused() {
        let mut image = Image::new(501, 401);
        for y in 0..401 {
            for x in 0..501 {
                image.data[y * 501 + x] = [x as f32; 3];
            }
        }
        let tables = LensTables {
            distortion: vec![0, 0, 1600, 0],
            ca_red: vec![0; 4],
            ca_blue: vec![0; 4],
            vignetting: vec![0; 4],
        };
        let out = correct(&image, &tables).unwrap();
        // The interior peak, not the corner or side midpoint, determines fill.
        let fill = 1.0 + 1600.0 / 16384.0;
        assert!((out.at(270, 200)[1] - (250.0 + 20.0 / fill)).abs() < 1e-4);
        let bad = LensTables { ca_red: vec![0; 3], ..tables };
        assert!(correct(&image, &bad).is_none());
    }

    #[test]
    fn neutral_tables_leave_the_image_unchanged() {
        let mut image = Image::new(64, 48);
        for (i, p) in image.data.iter_mut().enumerate() {
            *p = [(i % 7) as f32, (i % 5) as f32, (i % 3) as f32];
        }
        let tables =
            LensTables { distortion: vec![0; 16], ca_red: vec![0; 16], ca_blue: vec![0; 16], vignetting: vec![0; 16] };
        let out = correct(&image, &tables).unwrap();
        // A table no lens could have is refused, not turned into NaN.
        let broken = LensTables { distortion: vec![-16384; 16], ..tables.clone() };
        assert!(correct(&image, &broken).is_none());
        for (a, b) in image.data.iter().zip(&out.data) {
            for c in 0..3 {
                assert!((a[c] - b[c]).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn barrel_correction_pulls_edges_from_inside_and_brightens_corners() {
        let mut image = Image::new(200, 100);
        for y in 0..100 {
            for x in 0..200 {
                image.data[y * 200 + x] = [x as f32; 3];
            }
        }
        let ramp: Vec<i16> = (0..16).map(|i| -(i * 40)).collect();
        let vignetting: Vec<i16> = (0..16).map(|i| i * 800).collect();
        let out =
            correct(&image, &LensTables { distortion: ramp, ca_red: vec![0; 16], ca_blue: vec![0; 16], vignetting })
                .unwrap();
        // Centre untouched; the right edge now shows content from further in, brighter.
        assert!((out.at(100, 50)[0] - 100.0).abs() < 1.5);
        let source_x = 99.5 + (190.0 - 99.5) * 0.985;
        assert!(out.at(190, 50)[0] / source_x > 1.05);
        let flat = correct(
            &image,
            &LensTables {
                distortion: (0..16).map(|i| -(i * 40)).collect(),
                ca_red: vec![0; 16],
                ca_blue: vec![0; 16],
                vignetting: vec![0; 16],
            },
        )
        .unwrap();
        assert!(flat.at(190, 50)[0] < 189.5);
    }
}

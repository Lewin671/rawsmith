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
pub fn correct(image: &Image, tables: &LensTables) -> Image {
    let (w, h) = (image.width, image.height);
    let (cx, cy) = ((w as f32 - 1.0) * 0.5, (h as f32 - 1.0) * 0.5);
    let half_diagonal = (cx * cx + cy * cy).sqrt();

    // Source radius scale per channel: distortion, times the red/blue
    // magnification error for chromatic aberration.
    let scale = |ca: Option<&Vec<i16>>, fill: f32| {
        Radial(
            tables
                .distortion
                .iter()
                .enumerate()
                .map(|(i, &d)| {
                    let ca = ca.map_or(0.0, |t| t[i] as f32 / (1u32 << 21) as f32);
                    (1.0 + d as f32 / 16384.0) * (1.0 + ca) / fill
                })
                .collect(),
        )
    };
    // Zoom so the corrected picture exactly fills the frame: the frame edge
    // point that reaches furthest out in the source lands on the source edge.
    let edge_radius = cx.min(cy) / half_diagonal;
    let green = scale(None, 1.0);
    let fill = (0..=64).map(|i| green.at(edge_radius + (1.0 - edge_radius) * i as f32 / 64.0)).fold(f32::MIN, f32::max)
        * crate::tune("lens_fill", 1.0);
    let scales = [scale(Some(&tables.ca_red), fill), scale(None, fill), scale(Some(&tables.ca_blue), fill)];
    // Light falloff the lens caused, as a gain that undoes it.
    let gain = Radial(tables.vignetting.iter().map(|&v| ((v as f32 / 8192.0 - 1.0).exp2() - 0.5).exp2()).collect());

    let mut out = Image::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let dy = y as f32 - cy;
        for (x, p) in row.iter_mut().enumerate() {
            let dx = x as f32 - cx;
            let r = (dx * dx + dy * dy).sqrt() / half_diagonal;
            for c in 0..3 {
                let s = scales[c].at(r);
                let (sx, sy) = (cx + dx * s, cy + dy * s);
                // Vignetting is a property of where the light landed on the sensor.
                let source_r = r * s;
                p[c] = cubic(image, c, sx, sy).max(0.0) * gain.at(source_r);
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_tables_leave_the_image_unchanged() {
        let mut image = Image::new(64, 48);
        for (i, p) in image.data.iter_mut().enumerate() {
            *p = [(i % 7) as f32, (i % 5) as f32, (i % 3) as f32];
        }
        let tables =
            LensTables { distortion: vec![0; 16], ca_red: vec![0; 16], ca_blue: vec![0; 16], vignetting: vec![0; 16] };
        let out = correct(&image, &tables);
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
            correct(&image, &LensTables { distortion: ramp, ca_red: vec![0; 16], ca_blue: vec![0; 16], vignetting });
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
        );
        assert!(flat.at(190, 50)[0] < 189.5);
    }
}

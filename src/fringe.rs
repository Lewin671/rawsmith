//! Additional optional suppression of narrow, unsupported purple/blue edge colour.
//!
//! A pixel must be a chroma outlier across a strong luminance edge in both
//! directions. A purple surface with colour support on either side is left
//! alone. True subpixel purple detail is indistinguishable from false colour,
//! so this stronger post-filter is opt-in. Neutral-structure protection in
//! demosaicing runs independently of this control.

use rayon::prelude::*;

use crate::image::Image;

const LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593]; // Linear Rec.2020.

fn encoded(p: [f32; 3]) -> [f32; 3] {
    let y = p.iter().zip(LUMA).map(|(v, k)| v * k).sum::<f32>();
    let p = p.map(|v| v.max(0.0));
    let sum = (p[0] + p[1] + p[2]).max(1e-8);
    [(p[0] - p[1]) / sum, (p[2] - p[1]) / sum, y]
}

/// `strength` is 0..1. Works before tone, geometry and resizing; `radius`
/// is three image pixels at full resolution, two at half resolution.
/// Changes chroma only, preserving linear luminance and the edge profile.
pub fn suppress(image: &mut Image, strength: f32, radius: usize) {
    suppress_stripes(image, strength, radius, 64);
}

fn suppress_stripes(image: &mut Image, strength: f32, radius: usize, stripe_rows: usize) {
    if strength <= 0.0 {
        return;
    }
    let (w, h) = (image.width, image.height);
    let radius = radius.max(1).min(w.max(h));
    // Keep original encoded halos across stripes: reading already processed
    // rows would make the result depend on stripe size and scheduling.
    let mut previous = Vec::with_capacity(w * radius);
    let mut source = Vec::with_capacity(w * (stripe_rows + 2 * radius));
    for first in (0..h).step_by(stripe_rows) {
        let end = (first + stripe_rows).min(h);
        let base = first.saturating_sub(radius);
        source.clear();
        source.extend_from_slice(&previous);
        source.extend(image.data[first * w..(end + radius).min(h) * w].iter().copied().map(encoded));
        previous.clear();
        // On a stripe shorter than radius, retain the entire preceding halo.
        previous.extend_from_slice(&source[(end.saturating_sub(radius) - base) * w..(end - base) * w]);
        let at = |x: usize, y: usize| source[(y - base) * w + x];
        image.data[first * w..end * w].par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let y = y + first;
            for (x, p) in row.iter_mut().enumerate() {
                let centre = at(x, y);
                // Blue through magenta, with some red support; ordinary blue sky
                // and green/yellow edges are outside this range.
                if centre[0] < -0.05 || centre[1] < 0.02 || centre[2] <= 1e-8 {
                    continue;
                }
                // Three supported directions cover a surface quadrant, including
                // an object's corner. A narrow fringe has at most its two tangent
                // directions; do not mistake a purple surface corner for an outlier.
                let supported = [(-1isize, -1isize), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)]
                    .into_iter()
                    .filter(|&(dx, dy)| {
                        let (nx, ny) = (x as isize + dx * radius as isize, y as isize + dy * radius as isize);
                        if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                            return false;
                        }
                        let q = at(nx as usize, ny as usize);
                        (q[0] - centre[0]).abs() < 0.02 && (q[1] - centre[1]).abs() < 0.02
                    })
                    .count();
                if supported >= 3 {
                    continue;
                }
                let mut best = 0.0;
                let mut target = centre;
                for (dx, dy) in [(1isize, 0isize), (0, 1), (1, 1), (1, -1)] {
                    let (dx, dy) = (dx * radius as isize, dy * radius as isize);
                    let (ax, ay, bx, by) = (x as isize - dx, y as isize - dy, x as isize + dx, y as isize + dy);
                    if ax < 0
                        || ay < 0
                        || bx < 0
                        || by < 0
                        || ax >= w as isize
                        || bx >= w as isize
                        || ay >= h as isize
                        || by >= h as isize
                    {
                        continue;
                    }
                    let (a, b) = (at(ax as usize, ay as usize), at(bx as usize, by as usize));
                    let lo = centre[2].min(a[2]).min(b[2]);
                    let hi = centre[2].max(a[2]).max(b[2]);
                    if hi - lo < 0.25 * hi || hi < 1e-6 {
                        continue;
                    }
                    // Require TWO-sided lack of colour support. In particular a
                    // broad purple object against white has support on its inside.
                    let red_excess = centre[0] - a[0].max(b[0]);
                    let blue_excess = centre[1] - a[1].max(b[1]);
                    // Violet may have blue excess without red excess. Permit
                    // that only when red is not deficient relative to its support.
                    let excess = blue_excess.min(red_excess + 0.04);
                    if excess <= 0.02 {
                        continue;
                    }
                    // Transfer supported chroma, never average luminance. Use the
                    // endpoint closest in brightness when both have the same luma.
                    let t = if (b[2] - a[2]).abs() > 1e-8 {
                        ((centre[2] - a[2]) / (b[2] - a[2])).clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                    if excess > best {
                        best = excess;
                        target = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), centre[2]];
                    }
                }
                if best == 0.0 {
                    continue;
                }
                // Smooth onset above the chromaticity tolerance; no hard mask edge.
                let amount = strength * ((best - 0.02) / 0.02).clamp(0.0, 1.0);
                let a = centre[0] + amount * (target[0] - centre[0]);
                let b = centre[1] + amount * (target[1] - centre[1]);
                let g = (1.0 - a - b) / 3.0;
                let rgb = [g + a, g, g + b].map(|v| v.max(0.0));
                let y = rgb.iter().zip(LUMA).map(|(v, k)| v * k).sum::<f32>();
                *p = rgb.map(|v| v * centre[2] / y.max(1e-8));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripe_halos_match_an_immutable_full_frame() {
        let mut image = Image::new(47, 159);
        for y in 0..image.height {
            for x in 0..image.width {
                image.data[y * image.width + x] = if (x + y) % 17 < 2 {
                    [0.6, 0.15, 0.7]
                } else if (x + y) % 17 < 9 {
                    [0.03; 3]
                } else {
                    [1.0; 3]
                };
            }
        }
        for radius in [2, 3] {
            let mut reference = image.clone();
            suppress_stripes(&mut reference, 0.7, radius, image.height);
            assert_ne!(image.data, reference.data, "exercise active suppression");
            for rows in [1, 2, 7, 64] {
                let mut tiled = image.clone();
                suppress_stripes(&mut tiled, 0.7, radius, rows);
                assert_eq!(reference.data, tiled.data, "radius={radius}, rows={rows}");
            }
        }
    }

    #[test]
    fn removes_a_thin_purple_edge_without_changing_luminance() {
        let mut image = Image::new(40, 24);
        for y in 0..24 {
            for x in 0..40 {
                image.data[y * 40 + x] = if x < 18 {
                    [0.03; 3]
                } else if x < 20 {
                    [0.6, 0.15, 0.7]
                } else {
                    [1.0; 3]
                };
            }
        }
        let original = image.clone();
        suppress(&mut image, 1.0, 3);
        for (p, q) in image.data.iter().zip(&original.data) {
            assert!((encoded(*p)[2] - encoded(*q)[2]).abs() < 1e-6);
            assert!(p.iter().all(|v| v.is_finite() && *v >= 0.0));
        }
        for y in 3..21 {
            for x in 18..20 {
                let p = image.at(x, y);
                assert!((p[0] - p[1]).abs() < 1e-6 && (p[2] - p[1]).abs() < 1e-6, "{p:?}");
            }
        }
    }

    #[test]
    fn a_thin_blue_fringe_is_suppressed_too() {
        let mut image = Image::new(40, 24);
        for y in 0..24 {
            for x in 0..40 {
                image.data[y * 40 + x] = if x < 18 {
                    [0.03; 3]
                } else if x < 20 {
                    [0.15, 0.15, 0.7]
                } else {
                    [1.0; 3]
                };
            }
        }
        suppress(&mut image, 1.0, 3);
        let p = image.at(18, 12);
        assert!((p[2] - p[1]).abs() < 1e-6);
    }

    #[test]
    fn negative_working_channels_do_not_brighten_edges() {
        let mut image = Image::new(40, 24);
        for y in 0..24 {
            for x in 0..40 {
                image.data[y * 40 + x] = if x < 18 {
                    [0.03; 3]
                } else if x < 20 {
                    [0.6, -0.1, 0.7]
                } else {
                    [1.0; 3]
                };
            }
        }
        let original = image.clone();
        suppress(&mut image, 1.0, 3);
        assert!(image.at(18, 12)[1] >= 0.0, "the negative-green fringe must be processed");
        for (a, b) in image.data.iter().zip(&original.data) {
            let luma = |p: &[f32; 3]| p.iter().zip(LUMA).map(|(v, k)| v * k).sum::<f32>();
            assert!((luma(a) - luma(b)).abs() < 1e-6);
        }
    }

    #[test]
    fn supported_purple_surfaces_and_lighting_keep_their_colour() {
        let mut image = Image::new(80, 60);
        for y in 0..60 {
            for x in 0..80 {
                let purple = (20..60).contains(&x) && (15..45).contains(&y);
                image.data[y * 80 + x] = if purple { [0.7, 0.1, 0.8] } else { [0.9; 3] };
            }
        }
        let original = image.clone();
        suppress(&mut image, 1.0, 3);
        assert!(image.data.iter().zip(&original.data).all(|(a, b)| a == b));
        for (i, p) in image.data.iter_mut().enumerate() {
            let v = if i % 80 < 40 { 0.1 } else { 1.0 };
            *p = [0.7 * v, 0.1 * v, 0.8 * v];
        }
        let original = image.clone();
        suppress(&mut image, 1.0, 3);
        assert!(image.data.iter().zip(&original.data).all(|(a, b)| a == b));
    }

    #[test]
    fn disabled_is_exact_and_smooth_colour_without_edges_is_untouched() {
        let mut image = Image::new(20, 20);
        for (i, p) in image.data.iter_mut().enumerate() {
            *p = [0.5 + 0.1 * (i % 3) as f32, 0.2, 0.7];
        }
        let original = image.clone();
        suppress(&mut image, 0.0, 3);
        assert!(image.data.iter().zip(&original.data).all(|(a, b)| a == b));
        suppress(&mut image, 1.0, 3);
        assert!(image.data.iter().zip(&original.data).all(|(a, b)| a == b));
    }
}

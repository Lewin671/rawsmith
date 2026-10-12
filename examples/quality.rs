//! Ground-truth quality benchmark on synthetic scenes. Needs no photographs:
//!
//!     cargo run --release --example quality [-- --check]
//!
//! Prints one JSON document. Each section compares the pipeline's algorithm
//! with a naive baseline against a known correct answer.

#![allow(clippy::needless_range_loop)]

use rawsmith::color::{self, Spaces};
use rawsmith::demosaic::{self, Mosaic};
use rawsmith::image::Image;
use rawsmith::params::Params;
use rawsmith::pipeline::{self, Size};
use rawsmith::synthetic::Synthetic;
use rawsmith::{arw, tone};
use serde_json::{json, Value};

/// Small deterministic generator, so results are identical everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Approximately normal, unit variance.
    fn gauss(&mut self) -> f32 {
        (0..12).map(|_| self.next()).sum::<f32>() - 6.0
    }
}

fn psnr(a: &Image, b: &Image, margin: usize) -> f64 {
    let mut sum = 0.0f64;
    let mut n = 0u64;
    for y in margin..a.height - margin {
        for x in margin..a.width - margin {
            let (p, q) = (a.at(x, y), b.at(x, y));
            for c in 0..3 {
                // Compare in a perceptual (square-root) encoding.
                let d = (p[c].max(0.0).sqrt() - q[c].max(0.0).sqrt()) as f64;
                sum += d * d;
                n += 1;
            }
        }
    }
    10.0 * (1.0 / (sum / n as f64).max(1e-12)).log10()
}

/// Mean absolute error of the colour differences R-G and B-G: false colour.
fn false_colour(a: &Image, b: &Image, margin: usize) -> f64 {
    let mut sum = 0.0f64;
    let mut n = 0u64;
    for y in margin..a.height - margin {
        for x in margin..a.width - margin {
            let (p, q) = (a.at(x, y), b.at(x, y));
            sum += (((p[0] - p[1]) - (q[0] - q[1])).abs() + ((p[2] - p[1]) - (q[2] - q[1])).abs()) as f64;
            n += 2;
        }
    }
    sum / n as f64
}

fn mosaic_of(truth: &Image) -> Mosaic {
    let cfa = [0u8, 1, 1, 2];
    let data = (0..truth.width * truth.height)
        .map(|i| {
            let (x, y) = (i % truth.width, i / truth.width);
            truth.data[i][cfa[(y & 1) * 2 + (x & 1)] as usize]
        })
        .collect();
    Mosaic { width: truth.width, height: truth.height, data, cfa, clip: [f32::MAX; 3] }
}

/// Baseline: plain bilinear interpolation of each colour plane.
fn bilinear(m: &Mosaic) -> Image {
    let mut out = Image::new(m.width, m.height);
    for y in 0..m.height {
        for x in 0..m.width {
            let mut sum = [0.0f32; 3];
            let mut weight = [0.0f32; 3];
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= m.width as i32 || ny >= m.height as i32 {
                        continue;
                    }
                    let (nx, ny) = (nx as usize, ny as usize);
                    let c = m.cfa[(ny & 1) * 2 + (nx & 1)] as usize;
                    let w = if dx == 0 && dy == 0 {
                        4.0
                    } else if dx == 0 || dy == 0 {
                        2.0
                    } else {
                        1.0
                    };
                    sum[c] += m.data[ny * m.width + nx] * w;
                    weight[c] += w;
                }
            }
            let own = m.cfa[(y & 1) * 2 + (x & 1)] as usize;
            let mut p = [sum[0] / weight[0], sum[1] / weight[1], sum[2] / weight[2]];
            p[own] = m.data[y * m.width + x];
            out.data[y * m.width + x] = p;
        }
    }
    out
}

/// Baseline: Malvar-He-Cutler gradient-corrected linear interpolation.
fn mhc(m: &Mosaic) -> Image {
    let (w, h) = (m.width as i32, m.height as i32);
    let at = |x: i32, y: i32| {
        let mirror = |i: i32, n: i32| {
            if i < 0 {
                -i
            } else if i >= n {
                2 * n - 2 - i
            } else {
                i
            }
        };
        m.data[(mirror(y, h) * w + mirror(x, w)) as usize]
    };
    let mut out = Image::new(m.width, m.height);
    for y in 0..h {
        for x in 0..w {
            let own = m.cfa[((y & 1) * 2 + (x & 1)) as usize] as usize;
            let c = at(x, y);
            let cross = at(x - 1, y) + at(x + 1, y) + at(x, y - 1) + at(x, y + 1);
            let far = at(x - 2, y) + at(x + 2, y) + at(x, y - 2) + at(x, y + 2);
            let diagonal = at(x - 1, y - 1) + at(x + 1, y - 1) + at(x - 1, y + 1) + at(x + 1, y + 1);
            let mut p = [0.0f32; 3];
            p[own] = c;
            if own == 1 {
                let (horizontal, vertical) = (at(x - 1, y) + at(x + 1, y), at(x, y - 1) + at(x, y + 1));
                let (far_h, far_v) = (at(x - 2, y) + at(x + 2, y), at(x, y - 2) + at(x, y + 2));
                let along_row = (5.0 * c + 4.0 * horizontal - diagonal - far_h + 0.5 * far_v) / 8.0;
                let along_column = (5.0 * c + 4.0 * vertical - diagonal - far_v + 0.5 * far_h) / 8.0;
                let row_colour = m.cfa[((y & 1) * 2 + ((x + 1) & 1)) as usize] as usize;
                p[row_colour] = along_row;
                p[2 - row_colour] = along_column;
            } else {
                p[1] = (4.0 * c + 2.0 * cross - far) / 8.0;
                p[2 - own] = (6.0 * c + 2.0 * diagonal - 1.5 * far) / 8.0;
            }
            out.data[(y * w + x) as usize] = p;
        }
    }
    out
}

/// Test scenes with detail up to the sensor's resolution limit.
fn scenes() -> Vec<(&'static str, Image)> {
    let (w, h) = (512, 384);
    let mut out = Vec::new();

    // Zone plate: every orientation and every frequency up to Nyquist.
    let mut zone = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - w as f32 / 2.0, y as f32 - h as f32 / 2.0);
            // Local frequency grows with radius and reaches 0.4 cycles/pixel at the corner.
            let v = 0.5 + 0.4 * (0.0039 * (dx * dx + dy * dy)).cos();
            zone.data[y * w + x] = [v * 0.9, v, v * 0.8];
        }
    }
    out.push(("zone_plate", zone));

    // Thin coloured lines at several angles on a textured ground: branches.
    let mut lines = Image::new(w, h);
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let palette = [[0.75, 0.2, 0.1], [0.1, 0.5, 0.2], [0.15, 0.25, 0.8], [0.9, 0.85, 0.3], [0.02, 0.02, 0.03]];
    for y in 0..h {
        for x in 0..w {
            let ground = 0.45 + 0.05 * ((x as f32 * 0.05).sin() + (y as f32 * 0.07).cos());
            let mut p = [ground, ground * 0.98, ground * 1.05];
            for (k, colour) in palette.iter().enumerate() {
                let angle = 0.35 + k as f32 * 0.55;
                let d = (x as f32 * angle.cos() + y as f32 * angle.sin()).rem_euclid(37.0 + 9.0 * k as f32);
                // Anti-aliased line 1.6 px wide.
                let cover = (1.3 - (d - 4.0).abs()).clamp(0.0, 1.0);
                for c in 0..3 {
                    p[c] += (colour[c] - p[c]) * cover;
                }
            }
            lines.data[y * w + x] = p;
        }
    }
    out.push(("coloured_lines", lines));

    // Overlapping soft shapes with moderate colour, like petals or rocks.
    let mut blobs = Image::new(w, h);
    let spots: Vec<([f32; 2], f32, [f32; 3])> = (0..60)
        .map(|_| {
            let level = 0.08 + rng.next() * 0.7;
            let tint = [0.7 + rng.next() * 0.6, 0.7 + rng.next() * 0.6, 0.7 + rng.next() * 0.6];
            (
                [rng.next() * w as f32, rng.next() * h as f32],
                6.0 + rng.next() * 40.0,
                tint.map(|t| (level * t).min(1.0)),
            )
        })
        .collect();
    for y in 0..h {
        for x in 0..w {
            let mut p = [0.3f32, 0.32, 0.36];
            for (centre, radius, colour) in &spots {
                let d = ((x as f32 - centre[0]).powi(2) + (y as f32 - centre[1]).powi(2)).sqrt();
                let cover = ((radius - d) / 1.5).clamp(0.0, 1.0);
                for c in 0..3 {
                    p[c] += (colour[c] - p[c]) * cover;
                }
            }
            blobs.data[y * w + x] = p;
        }
    }
    out.push(("blobs", blobs));

    // Fractal texture with slowly varying tint: foliage, snow, gravel.
    let lattice: Vec<f32> = (0..7 * 4096).map(|_| rng.next()).collect();
    let noise = |x: f32, y: f32, octave: usize| {
        let at = |ix: usize, iy: usize| lattice[octave * 4096 + (iy % 64) * 64 + ix % 64];
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (ix, iy) = (x0 as usize, y0 as usize);
        let top = at(ix, iy) + (at(ix + 1, iy) - at(ix, iy)) * fx;
        let bottom = at(ix, iy + 1) + (at(ix + 1, iy + 1) - at(ix, iy + 1)) * fx;
        top + (bottom - top) * fy
    };
    let mut texture = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32, y as f32);
            // Octaves from 64 px features down to 1.5 px.
            let level: f32 = (0..6)
                .map(|o| {
                    (noise(fx / 64.0 * 1.9f32.powi(o as i32), fy / 64.0 * 1.9f32.powi(o as i32), o) - 0.5)
                        * 0.75f32.powi(o as i32)
                })
                .sum();
            let level = (0.35 + 0.45 * level).clamp(0.02, 0.95);
            let warm = noise(fx / 90.0, fy / 90.0, 6) - 0.5;
            texture.data[y * w + x] = [level * (1.0 + 0.4 * warm), level, level * (1.0 - 0.4 * warm)];
        }
    }
    out.push(("fractal_texture", texture));
    out
}

fn demosaic_section() -> Value {
    let mut rows = Vec::new();
    let mut gains = [0.0, 0.0];
    let all = scenes();
    for (name, truth) in &all {
        let m = mosaic_of(truth);
        let results = [demosaic::full(&m), mhc(&m), bilinear(&m)];
        let p: Vec<f64> = results.iter().map(|r| psnr(r, truth, 4)).collect();
        let f: Vec<f64> = results.iter().map(|r| false_colour(r, truth, 4)).collect();
        gains[0] += (p[0] - p[2]) / all.len() as f64;
        gains[1] += (p[0] - p[1]) / all.len() as f64;
        rows.push(json!({
            "scene": name,
            "psnr_db": {"rawsmith": round(p[0], 2), "malvar_he_cutler": round(p[1], 2), "bilinear": round(p[2], 2)},
            "false_colour": {"rawsmith": round(f[0], 5), "malvar_he_cutler": round(f[1], 5), "bilinear": round(f[2], 5)},
        }));
    }
    json!({"scenes": rows, "mean_psnr_gain_db": {"over_bilinear": round(gains[0], 2), "over_malvar_he_cutler": round(gains[1], 2)}})
}

/// A bright warm gradient whose green (and then red) channels saturate.
fn highlight_section() -> Value {
    let (w, h) = (256, 192);
    let wb = [2.5f32, 1.0, 1.6];
    let mut truth = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            // White-balanced camera RGB: an orange glow brightening to the right.
            let level = 0.2 + 2.2 * (x as f32 / w as f32).powi(2) * (1.0 - 0.3 * (y as f32 / h as f32));
            truth.data[y * w + x] = [level * 1.25, level, level * 0.55];
        }
    }
    let mut m = mosaic_of(&truth);
    m.clip = wb;
    for (i, v) in m.data.iter_mut().enumerate() {
        let c = m.cfa[((i / w) & 1) * 2 + ((i % w) & 1)] as usize;
        *v = v.min(wb[c]);
    }
    let clipped = demosaic::full(&m);
    let mut rebuilt = clipped.clone();
    let fraction = demosaic::reconstruct_highlights(&mut rebuilt, m.clip);
    // Error over pixels where exactly one or two channels saturated: those
    // are recoverable in principle.
    let error = |image: &Image| {
        let (mut sum, mut n) = (0.0f64, 0u64);
        for i in 0..w * h {
            let t = truth.data[i];
            let lost = (0..3).filter(|&c| t[c] >= wb[c]).count();
            if lost == 1 || lost == 2 {
                for c in 0..3 {
                    sum += ((image.data[i][c] - t[c]).abs() / t[c]) as f64;
                    n += 1;
                }
            }
        }
        sum / n.max(1) as f64
    };
    json!({
        "clipped_fraction": round(fraction as f64, 3),
        "mean_relative_error": {"rawsmith": round(error(&rebuilt), 4), "clip_only": round(error(&clipped), 4)},
    })
}

/// Known colours through the whole RAW path: file, decode, white balance,
/// demosaic, camera matrix. Reports the Oklab colour difference (x100,
/// roughly comparable to CIE Delta E).
fn colour_section() -> Value {
    let cam = color::camera_matrix("ILCE-7M4").unwrap();
    let spaces = Spaces::new();
    // D65 white as the camera sees it defines the white balance.
    let white = color::apply(&cam, color::apply(&color::REC2020_TO_XYZ, [1.0; 3]));
    let levels = [(1024.0 * white[1] / white[0]) as i16, 1024, 1024, (1024.0 * white[1] / white[2]) as i16];
    let patches: Vec<[f32; 3]> = {
        let mut rng = Rng(42);
        let mut list = vec![[0.18; 3], [0.02; 3], [0.6; 3]];
        list.extend((0..21).map(|_| {
            color::apply(
                &spaces.srgb_to_work,
                [rng.next() * 0.6 + 0.02, rng.next() * 0.6 + 0.02, rng.next() * 0.6 + 0.02],
            )
        }));
        list
    };
    let (cell, columns) = (32, 6);
    let (w, h) = (cell * columns, cell * patches.len() / columns);
    let mut file = Synthetic::from_camera_rgb(w, h, |x, y| {
        let patch = patches[(y / cell) * columns + x / cell];
        // Scene values to sensor values: undo the pipeline's base exposure.
        color::apply(&cam, color::apply(&color::REC2020_TO_XYZ, patch)).map(|v| v * 0.5)
    });
    file.wb_levels = levels;
    let raw = arw::decode(&file.to_bytes()).unwrap();
    let neutral = Params { lens_corrections: false, denoise: 0.0, ..Params::default() };
    let scene = pipeline::scene(&raw, &neutral, Size::default()).unwrap();
    let mut errors: Vec<f64> = Vec::new();
    for (i, patch) in patches.iter().enumerate() {
        let (x, y) = ((i % columns) * cell + cell / 2, (i / columns) * cell + cell / 2);
        // Sensor green defines the raw scale, so scene values carry the
        // camera's green response to white as a constant factor.
        let got = scene.image.at(x, y).map(|v| v * 2.0 / white[1]);
        let (a, b) = (spaces.to_oklab(got), spaces.to_oklab(*patch));
        errors.push(((0..3).map(|c| (a[c] - b[c]).powi(2)).sum::<f32>().sqrt() * 100.0) as f64);
    }
    json!({
        "patches": patches.len(),
        "mean_delta_e": round(errors.iter().sum::<f64>() / errors.len() as f64, 3),
        "max_delta_e": round(errors.iter().cloned().fold(0.0, f64::max), 3),
    })
}

/// Sharpening on a clean step edge: how much the edge steepens and how far
/// it overshoots.
fn tone_section() -> Value {
    let spaces = Spaces::new();
    let (w, h) = (128, 32);
    let mut step = Image::new(w, h);
    for (i, p) in step.data.iter_mut().enumerate() {
        // A slightly soft edge between two greys.
        let t = ((i % w) as f32 - 63.5).clamp(-1.5, 1.5) / 3.0 + 0.5;
        *p = [0.05 + 0.25 * t; 3];
    }
    let render = |sharpen: f64| pipeline::finish(&step, &Params { sharpen, ..Params::default() }, &spaces);
    let (soft, sharp) = (render(0.0), render(40.0));
    let row = |image: &Image| (0..w).map(|x| image.at(x, h / 2)[1]).collect::<Vec<f32>>();
    let (a, b) = (row(&soft), row(&sharp));
    let (lo, hi) = (a[8], a[w - 8]);
    let overshoot = b.iter().map(|&v| (v - hi).max(lo - v).max(0.0)).fold(0.0, f32::max) / (hi - lo);
    let slope = |r: &[f32]| r.windows(2).map(|p| p[1] - p[0]).fold(0.0, f32::max);

    // The filmic curve must be monotone for every contrast setting.
    let monotone = [-100.0, -50.0, 0.0, 50.0, 100.0].iter().all(|&contrast| {
        let f = tone::Filmic::new(contrast, 1.5);
        (1..4000).all(|i| f.eval(i as f32 / 1000.0) >= f.eval((i - 1) as f32 / 1000.0))
    });
    json!({
        "sharpen_edge_slope_gain": round((slope(&b) / slope(&a)) as f64, 2),
        "sharpen_overshoot_pct_of_step": round(overshoot as f64 * 100.0, 2),
        "tone_curve_monotone": monotone,
    })
}

/// Noise reduction on a noisy flat-and-edge scene.
fn denoise_section() -> Value {
    let (w, h) = (384, 256);
    let mut rng = Rng(7);
    let clean = |x: usize, _y: usize| if x < w / 2 { [0.04f32, 0.05, 0.03] } else { [0.14, 0.12, 0.16] };
    let cam = color::camera_matrix("ILCE-7M4").unwrap();
    let noise: Vec<f32> = (0..w * h).map(|_| rng.gauss()).collect();
    let file = Synthetic::from_camera_rgb(w, h, |x, y| {
        // Shot-noise-like: standard deviation grows with the square root of the signal.
        let c = color::apply(&cam, color::apply(&color::REC2020_TO_XYZ, clean(x, y)));
        c.map(|v| (v * 0.3 + (v * 0.3).sqrt() * 0.02 * noise[y * w + x]).max(0.0))
    });
    let raw = arw::decode(&file.to_bytes()).unwrap();
    let render = |denoise: f64| {
        pipeline::scene(&raw, &Params { denoise, lens_corrections: false, ..Params::default() }, Size::default())
            .unwrap()
            .image
    };
    let (noisy, smooth) = (render(0.0), render(40.0));
    // Chroma noise in a flat area, and the width of the edge between the halves.
    let chroma_noise = |image: &Image| {
        let values: Vec<f32> = (40..h - 40)
            .flat_map(|y| (40..w / 2 - 40).map(move |x| (x, y)))
            .map(|(x, y)| {
                let p = image.at(x, y);
                p[0] - p[1]
            })
            .collect();
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        (values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / values.len() as f32).sqrt()
    };
    // How much of the step between the halves survives right at the edge.
    let edge = |image: &Image| {
        let column = |x: usize| (40..h - 40).map(|y| image.at(x, y)[1]).sum::<f32>() / (h - 80) as f32;
        (column(w / 2 + 1) - column(w / 2 - 2)) / (column(3 * w / 4) - column(w / 4))
    };
    json!({
        "chroma_noise_reduction_pct": round((1.0 - chroma_noise(&smooth) / chroma_noise(&noisy)) as f64 * 100.0, 1),
        "edge_contrast_kept_pct": round((edge(&smooth) / edge(&noisy)) as f64 * 100.0, 1),
    })
}

fn round(v: f64, places: i32) -> f64 {
    let k = 10f64.powi(places);
    (v * k).round() / k
}

/// Minimum quality the pipeline must keep. `--check` fails when a change
/// regresses below any of these.
fn failures(report: &Value) -> Vec<String> {
    let number = |path: &str| path.split('.').fold(report, |v, key| &v[key]).as_f64().unwrap_or(f64::NAN);
    let floor = |path: &str, min: f64| {
        (number(path).is_nan() || number(path) < min).then(|| format!("{path} = {} is below {min}", number(path)))
    };
    let ceiling = |path: &str, max: f64| {
        (number(path).is_nan() || number(path) > max).then(|| format!("{path} = {} is above {max}", number(path)))
    };
    [
        floor("demosaic.mean_psnr_gain_db.over_bilinear", 4.0),
        floor("demosaic.mean_psnr_gain_db.over_malvar_he_cutler", 1.0),
        ceiling("highlight_reconstruction.mean_relative_error.rawsmith", 0.03),
        ceiling("colour_accuracy.mean_delta_e", 0.5),
        ceiling("colour_accuracy.max_delta_e", 1.0),
        ceiling("tone_and_sharpening.sharpen_overshoot_pct_of_step", 2.0),
        floor("tone_and_sharpening.sharpen_edge_slope_gain", 1.02),
        floor("noise_reduction.chroma_noise_reduction_pct", 25.0),
        floor("noise_reduction.edge_contrast_kept_pct", 95.0),
        (report["tone_and_sharpening"]["tone_curve_monotone"] != true)
            .then(|| "tone curve is not monotone".to_string()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn main() {
    let report = json!({
        "rawsmith": env!("CARGO_PKG_VERSION"),
        "demosaic": demosaic_section(),
        "highlight_reconstruction": highlight_section(),
        "colour_accuracy": colour_section(),
        "tone_and_sharpening": tone_section(),
        "noise_reduction": denoise_section(),
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if std::env::args().any(|a| a == "--check") {
        let failed = failures(&report);
        for failure in &failed {
            eprintln!("quality regression: {failure}");
        }
        if !failed.is_empty() {
            std::process::exit(1);
        }
    }
}

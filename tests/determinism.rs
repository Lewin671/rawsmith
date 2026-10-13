use std::path::Path;

use rawsmith::arw::{self, RawImage};
use rawsmith::color::Spaces;
use rawsmith::image::Image;
use rawsmith::params::Params;
use rawsmith::pipeline::{self, Size};
use rawsmith::{auto, output, stats, synthetic::Synthetic};
use rayon::ThreadPoolBuilder;
use serde_json::json;

/// Haze over textured dark ground, a coloured sky and a sun with partially
/// clipped channels. Larger than the tone base layer, with an uneven crop,
/// so both full-resolution and reduced analysis/filtering paths are exercised.
fn hazy_sunset() -> RawImage {
    let (w, h) = (960, 640);
    let synthetic = Synthetic::from_camera_rgb(w, h, |x, y| {
        let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
        let ground = if v > 0.58 + 0.025 * (u * 31.0).sin() { 0.008 } else { 0.10 + 0.15 * (1.0 - v) };
        let texture = ((x * 17 + y * 29) % 101) as f32 / 101.0 * 0.008;
        let distance = ((u - 0.72).powi(2) + (v - 0.32).powi(2)).sqrt();
        let sun = (1.0 - distance / 0.12).max(0.0).powi(2) * 4.0;
        [
            0.035 + ground * 0.9 + texture + sun * 0.75,
            0.045 + ground * 0.8 + texture + sun,
            0.055 + ground * 0.6 + texture + sun * 0.9,
        ]
    });
    arw::decode(&synthetic.to_bytes()).unwrap()
}

fn recipe() -> Params {
    // The reported sparse recipe, including two opposing exposure gradients.
    serde_json::from_value(json!({
        "exposure": -0.55, "contrast": 8, "highlights": -25,
        "dehaze": 6, "clarity": 3, "vibrance": 18,
        "sharpen": 30, "denoise": 12,
        "crop": {"top": 0.12, "bottom": 0.90},
        "gradients": [
            {"from": [0.5, 0.0], "to": [0.5, 0.5], "exposure": -0.15},
            {"from": [0.5, 1.0], "to": [0.5, 0.6], "exposure": 0.18}
        ]
    }))
    .unwrap()
}

fn pixel_bits(image: &Image) -> Vec<[u32; 3]> {
    assert!(image.data.iter().flatten().all(|v| v.is_finite()));
    image.data.iter().map(|p| p.map(f32::to_bits)).collect()
}

struct Render {
    scene: Vec<[u32; 3]>,
    pixels: Vec<[u32; 3]>,
    report: Vec<u8>,
    jpeg: Vec<u8>,
    png: Vec<u8>,
}

fn render(raw: &RawImage, params: &Params, edge: Option<usize>) -> Render {
    let scene = pipeline::scene(raw, params, Size { max_edge: edge }).unwrap();
    assert!(scene.sensor_clipped_pct > 0.0, "the sun must exercise highlight reconstruction");
    let spaces = Spaces::new();
    let image = pipeline::render(raw, params, Size { max_edge: edge }).unwrap().image;
    Render {
        scene: pixel_bits(&scene.image),
        pixels: pixel_bits(&image),
        // The same fields as a render report, except paths and elapsed_ms.
        report: serde_json::to_vec(&json!({
            "recipe": params.to_sparse(),
            "white_balance": scene.white_balance,
            "sensor_clipped_pct": scene.sensor_clipped_pct,
            "stats": stats::image_stats(&image, &spaces),
            "raw_stats": stats::raw_stats(raw),
        }))
        .unwrap(),
        jpeg: output::encode(Path::new("synthetic.jpg"), &image, 94).unwrap(),
        png: output::encode(Path::new("synthetic.png"), &image, 94).unwrap(),
    }
}

fn assert_pixels_equal(want: &[[u32; 3]], got: &[[u32; 3]], context: &str) {
    assert_eq!(want.len(), got.len(), "{context}: pixel count");
    if let Some(i) = want.iter().zip(got).position(|(a, b)| a != b) {
        panic!("{context}: pixel {i} differs: {:?} != {:?}", want[i], got[i]);
    }
}

#[test]
fn render_pixels_reports_and_exports_ignore_rayon_scheduling() {
    let raw = hazy_sunset();
    let params = recipe();
    params.validate().unwrap();
    let pools: Vec<_> = [1, 2, 3, 8]
        .into_iter()
        .map(|threads| ThreadPoolBuilder::new().num_threads(threads).build().unwrap())
        .collect();
    // A control without dehaze isolates its brightness compensation from
    // demosaicing, highlight reconstruction, denoise and local contrast.
    for params in [Params { dehaze: 0.0, ..params.clone() }, params.clone(), Params { denoise: 85.0, ..params }] {
        for edge in [None, Some(400)] {
            let reference = pools[0].install(|| render(&raw, &params, edge));
            for pool in &pools {
                for repeat in 0..3 {
                    let got = pool.install(|| render(&raw, &params, edge));
                    let context = format!(
                        "dehaze {}, edge {edge:?}, threads {}, repeat {repeat}",
                        params.dehaze,
                        pool.current_num_threads()
                    );
                    assert_pixels_equal(&reference.scene, &got.scene, &format!("{context}: scene"));
                    assert_pixels_equal(&reference.pixels, &got.pixels, &context);
                    assert_eq!(reference.report, got.report, "{context}: report");
                    assert!(reference.jpeg == got.jpeg, "{context}: JPEG bytes");
                    assert!(reference.png == got.png, "{context}: PNG bytes");
                }
            }
        }
    }
}

#[test]
fn auto_recipe_and_decisions_ignore_rayon_scheduling() {
    // A subdued atmospheric veil with distant texture and a contrasty
    // foreground. Uniform coloured gradients alone must no longer trigger haze.
    let raw = arw::decode(
        &Synthetic::from_camera_rgb(640, 480, |x, y| {
            let texture = if (x / 20 + y / 20) % 2 == 0 { -1.0 } else { 1.0 };
            let level = if y < 320 { 0.12 + texture * 0.015 } else { 0.055 + texture * 0.045 };
            let (r, b) = if y < 320 { (0.70, 1.15) } else { (1.0, 1.0) };
            [level * r / (2600.0 / 1024.0), level, level * b / (1600.0 / 1024.0)]
        })
        .to_bytes(),
    )
    .unwrap();
    let reference = ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap()
        .install(|| auto::auto(&raw, &Params::default()).unwrap());
    assert!(reference.params.dehaze > 0.0, "auto must evaluate dehaze candidates");
    for threads in [1, 2, 3, 8] {
        let pool = ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        for repeat in 0..2 {
            let got = pool.install(|| auto::auto(&raw, &Params::default()).unwrap());
            assert_eq!(reference.params, got.params, "threads {threads}, repeat {repeat}: auto recipe");
            assert_eq!(reference.notes, got.notes, "threads {threads}, repeat {repeat}: auto decisions");
        }
    }
}

//! The CLI must resize finished pixels, including geometry and local edits.
use std::io::Cursor;
use std::path::Path;
use std::process::Command;

use rawsmith::image::Image;
use rawsmith::synthetic::Synthetic;
use serde_json::json;

fn read_png(path: &Path) -> Image {
    let mut reader = png::Decoder::new(Cursor::new(std::fs::read(path).unwrap())).read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).unwrap();
    assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
    Image {
        width: info.width as usize,
        height: info.height as usize,
        data: buffer[..info.buffer_size()]
            .as_chunks::<6>()
            .0
            .iter()
            .map(|p| [0, 2, 4].map(|i| u16::from_be_bytes([p[i], p[i + 1]]) as f32 / 65535.0))
            .collect(),
    }
}

#[test]
fn preview_and_comparison_resize_the_finished_export() {
    let dir = std::env::temp_dir().join(format!("rawsmith-preview-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let raw = dir.join("synthetic.ARW");
    let recipe = dir.join("recipe.json");
    let full = dir.join("full.png");
    let small = dir.join("small.png");
    let compare = dir.join("compare.png");
    let mut synthetic = Synthetic::from_camera_rgb(322, 214, |x, y| {
        let noise = ((x * 31 + y * 47) % 97) as f32 / 97.0 * 0.015;
        let v = if x < 161 { 0.015 } else { 0.14 } + noise;
        if (159..161).contains(&x) {
            [0.16, 0.025, 0.22]
        } else {
            [v * 0.8, v, v * 0.6]
        }
    });
    synthetic.orientation = 6;
    std::fs::write(&raw, synthetic.to_bytes()).unwrap();
    std::fs::write(
        &recipe,
        serde_json::to_vec(&json!({
            "denoise": 85, "defringe": 100, "sharpen": 65, "clarity": 20, "dehaze": 15,
            "highlights": -20, "shadows": 30, "rotate": 3.0,
            "crop": {"top": 0.07, "bottom": 0.93, "left": 0.11},
            "vignette": -15,
            "masks": [
                {"shape":"radial","center":[0.4,0.6],"radii":[0.3,0.2],"rotation":35,"exposure":0.7,"shadows":20,"contrast":15,"saturation":-20},
                {"from":[0.5,0],"to":[0.5,0.7],"luma":[0.65,1],"highlights":-30}
            ],
            "gradients": [{"from": [0.5, 0.0], "to": [0.5, 0.7], "exposure": -0.4}]
        }))
        .unwrap(),
    )
    .unwrap();
    let run = |command: &str, out: &Path, size: Option<usize>| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rawsmith"));
        cmd.arg(command).arg(&raw).arg("-o").arg(out);
        if command == "compare" {
            cmd.arg("--panel").arg(format!("recipe:{}", recipe.display()));
        } else {
            cmd.arg("--recipe").arg(&recipe);
        }
        if let Some(size) = size {
            cmd.arg("--size").arg(size.to_string());
        }
        let output = cmd.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.stderr.is_empty());
    };
    run("render", &full, None);
    let full_image = read_png(&full);
    // The recipe must exercise suppression rather than merely accepting it.
    let output = Command::new(env!("CARGO_BIN_EXE_rawsmith"))
        .arg("render")
        .arg(&raw)
        .arg("--recipe")
        .arg(&recipe)
        .args(["--set", "defringe=0", "-o"])
        .arg(&small)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_ne!(full_image.data, read_png(&small).data, "suppression must change this scene");
    // Region extraction retains the full frame's filter support, including
    // the opt-in fringe and high-strength low-frequency denoise stages.
    let output = Command::new(env!("CARGO_BIN_EXE_rawsmith"))
        .arg("render")
        .arg(&raw)
        .arg("--recipe")
        .arg(&recipe)
        .args(["--region", "0.25,0.25,0.75,0.75", "-o"])
        .arg(&compare)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let expected = full_image.crop(
        full_image.width / 4,
        full_image.height / 4,
        (full_image.width / 2).max(1),
        (full_image.height / 2).max(1),
    );
    assert_eq!(read_png(&compare).data, expected.data);
    for size in [1, 83, 161, 240, 1000] {
        run("render", &small, Some(size));
        let small_image = read_png(&small);
        let reference = full_image.resized(small_image.width, small_image.height);
        for (p, q) in small_image.data.iter().zip(reference.data) {
            for c in 0..3 {
                assert!((p[c] - q[c]).abs() <= 1.5 / 65535.0);
            }
        }
        run("compare", &compare, Some(size));
        assert_eq!(std::fs::read(&small).unwrap(), std::fs::read(&compare).unwrap());
    }
    // A different aspect ratio forces the first panel below its --size bound.
    // It must be resized once, directly to the final common height.
    let alternate = dir.join("alternate.json");
    std::fs::write(&alternate, br#"{"crop":{"top":0.6}}"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rawsmith"))
        .arg("compare")
        .arg(&raw)
        .arg("-o")
        .arg(&compare)
        .arg("--panel")
        .arg(format!("recipe:{}", recipe.display()))
        .arg("--panel")
        .arg(format!("recipe:{}", alternate.display()))
        .arg("--size")
        .arg("83")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let (w, h) = (
        report["panels"][0]["width"].as_u64().unwrap() as usize,
        report["panels"][0]["height"].as_u64().unwrap() as usize,
    );
    for panel in report["panels"].as_array().unwrap() {
        assert!(panel["width"].as_u64().unwrap() <= 83);
        assert!(panel["height"].as_u64().unwrap() <= 83);
    }
    let reference = full_image.resized(w, h);
    let panel = read_png(&compare).crop(0, 0, w, h);
    for (p, q) in panel.data.iter().zip(reference.data) {
        for c in 0..3 {
            assert!((p[c] - q[c]).abs() <= 1.5 / 65535.0);
        }
    }
    let diagnostic = |out: &Path, extra: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_rawsmith"))
            .arg("render")
            .arg(&raw)
            .arg("--recipe")
            .arg(&recipe)
            .args(["--show-mask", "1", "-o"])
            .arg(out)
            .args(extra)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    };
    diagnostic(&full, &[]);
    let mask = read_png(&full);
    diagnostic(&small, &["--size", "83"]);
    let preview = read_png(&small);
    let expected = mask.resized(preview.width, preview.height);
    for (p, q) in preview.data.iter().zip(expected.data) {
        assert!((p[0] - q[0]).abs() <= 1.5 / 65535.0);
    }
    diagnostic(&small, &["--region", "0.25,0.25,0.75,0.75"]);
    assert_eq!(read_png(&small).data, mask.crop(mask.width / 4, mask.height / 4, mask.width / 2, mask.height / 2).data);
    std::fs::remove_dir_all(dir).unwrap();
}

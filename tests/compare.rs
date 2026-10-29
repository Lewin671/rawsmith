//! Comparison acceptance uses synthetic photos only, including JPEG declarations.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use rawsmith::{image::Image, output, synthetic::Synthetic};
use serde_json::{json, Value};

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rawsmith-compare-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn raw(&self, orientation: u16, jpeg: Option<&[u8]>) -> PathBuf {
        self.raw_with_colour(orientation, jpeg, None, None)
    }

    fn raw_with_colour(
        &self,
        orientation: u16,
        jpeg: Option<&[u8]>,
        space: Option<u16>,
        interop: Option<[u8; 4]>,
    ) -> PathBuf {
        let mut synthetic =
            Synthetic::from_camera_rgb(96, 64, |x, y| [0.01 + x as f32 / 2000.0, 0.02 + y as f32 / 1000.0, 0.03]);
        synthetic.orientation = orientation;
        synthetic.exif_color_space = space;
        synthetic.interoperability_index = interop;
        synthetic.embedded_jpeg = jpeg.map(Vec::from);
        let raw = synthetic.to_bytes();
        let path = self.path("frame.ARW");
        std::fs::write(&path, raw).unwrap();
        path
    }

    fn recipe(&self, name: &str, recipe: Value) -> PathBuf {
        let path = self.path(name);
        std::fs::write(&path, serde_json::to_vec(&recipe).unwrap()).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn run(args: &[&str], success: bool) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_rawsmith")).args(args).output().unwrap();
    assert_eq!(output.status.success(), success, "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
    if success {
        assert!(output.stderr.is_empty());
        serde_json::from_slice(&output.stdout).unwrap()
    } else {
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        serde_json::from_slice(&output.stderr).unwrap()
    }
}

fn jpeg(width: usize, height: usize, srgb: bool) -> Vec<u8> {
    let image = Image {
        width,
        height,
        data: (0..width * height)
            .map(|i| [(i % width) as f32 / width as f32, (i / width) as f32 / height as f32, 0.25])
            .collect(),
    };
    output::encode_with_color_space(Path::new("camera.jpg"), &image, 98, srgb).unwrap()
}

fn decoded_jpeg(jpeg: &[u8]) -> Image {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(jpeg));
    let bytes = decoder.decode().unwrap();
    let info = decoder.info().unwrap();
    Image {
        width: info.width as usize,
        height: info.height as usize,
        data: bytes.as_chunks::<3>().0.iter().map(|p| p.map(|v| v as f32 / 255.0)).collect(),
    }
}

fn png(path: &Path) -> (Image, bool) {
    let decoder = png::Decoder::new(Cursor::new(std::fs::read(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let srgb = reader.info().srgb.is_some();
    let mut bytes = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
    let image = Image {
        width: info.width as usize,
        height: info.height as usize,
        data: bytes[..info.buffer_size()]
            .as_chunks::<6>()
            .0
            .iter()
            .map(|p| [0, 2, 4].map(|i| u16::from_be_bytes([p[i], p[i + 1]]) as f32 / 65535.0))
            .collect(),
    };
    (image, srgb)
}

#[test]
fn camera_pixels_are_upright_and_default_report_is_ordered() {
    let f = Fixture::new("orientations");
    let embedded = jpeg(96, 64, true);
    let camera = decoded_jpeg(&embedded);
    let output = f.path("comparison.png");
    for orientation in [1, 3, 6, 8] {
        let raw = f.raw(orientation, Some(&embedded));
        let original = std::fs::read(&raw).unwrap();
        let report = run(&["compare", s(&raw), "-o", s(&output)], true);
        let (w, h) = if matches!(orientation, 6 | 8) { (64, 96) } else { (96, 64) };
        assert_eq!(report["panels"][0]["kind"], "camera");
        assert_eq!(report["panels"][1]["kind"], "saved");
        assert_eq!(report["panels"][1]["recipe_file"], Value::Null);
        assert_eq!(report["panels"][0]["width"], w);
        assert_eq!(report["panels"][0]["height"], h);
        assert_eq!(report["panels"][1]["width"], w);
        assert_eq!(report["panels"][1]["height"], h);
        assert_eq!(report["width"], w * 2 + 2);
        assert_eq!(report["camera_framing_panel"], 1);
        assert_eq!(report["output_color_space"], "sRGB");
        assert_eq!(report["output_replaced"], orientation != 1);
        assert_eq!(report["warnings"], json!([]));
        let (image, srgb) = png(&output);
        assert!(srgb);
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w / 3, h / 3)] {
            let (sx, sy) = match orientation {
                3 => (95 - x, 63 - y),
                6 => (y, 63 - x),
                8 => (95 - y, x),
                _ => (x, y),
            };
            for (got, want) in image.at(x, y).into_iter().zip(camera.at(sx, sy)) {
                assert!((got - want).abs() < 1.0 / 65535.0, "orientation {orientation}: {x},{y}");
            }
        }
        for y in 0..h {
            for x in w..w + 2 {
                assert!(image.at(x, y).into_iter().all(|v| (v - 0.5).abs() < 1.0 / 65535.0));
            }
        }
        // A comparison's developed panel agrees with the existing render command.
        let render = f.path("render.png");
        run(&["render", s(&raw), "-o", s(&render), "--size", "1400"], true);
        let (developed, _) = png(&render);
        for y in 0..h {
            for x in 0..w {
                assert_eq!(image.at(w + 2 + x, y), developed.at(x, y));
            }
        }
        assert_eq!(std::fs::read(raw).unwrap(), original);
        assert!(!f.path("frame.ARW.rawsmith.json").exists());
    }
}

#[test]
fn single_camera_is_upright_bounded_and_ignores_saved_framing() {
    let f = Fixture::new("single-camera");
    let embedded = jpeg(96, 64, true);
    let original = decoded_jpeg(&embedded);
    let recipe = f.recipe("frame.ARW.rawsmith.json", json!({"crop": {"top": 0.5}, "rotate": 10}));
    let saved = std::fs::read(&recipe).unwrap();
    for orientation in [1, 3, 6, 8] {
        let raw = f.raw(orientation, Some(&embedded));
        let output = f.path("camera.png");
        let report = run(&["compare", s(&raw), "--panel", "camera", "-o", s(&output), "--size", "48"], true);
        let (image, tagged) = png(&output);
        let (width, height) = if matches!(orientation, 6 | 8) { (64, 96) } else { (96, 64) };
        let expected = Image {
            width,
            height,
            data: (0..width * height)
                .map(|i| {
                    let (x, y) = (i % width, i / width);
                    let (sx, sy) = match orientation {
                        3 => (95 - x, 63 - y),
                        6 => (y, 63 - x),
                        8 => (95 - y, x),
                        _ => (x, y),
                    };
                    original.at(sx, sy)
                })
                .collect(),
        };
        let expected = if matches!(orientation, 6 | 8) { expected.resized(32, 48) } else { expected.resized(48, 32) };
        assert_eq!(report["panels"].as_array().unwrap().len(), 1);
        assert_eq!(report["width"], image.width);
        assert_eq!(report["height"], image.height);
        assert_eq!(report["camera_framing_panel"], Value::Null);
        assert_eq!(report["framing_matched"], true);
        assert_eq!(report["warnings"], json!([]));
        assert_eq!(report["output_color_space"], "sRGB");
        assert!(tagged);
        assert_eq!((image.width, image.height), (expected.width, expected.height));
        for (actual, expected) in image.data.iter().zip(&expected.data) {
            assert!(actual.iter().zip(expected).all(|(a, b)| (a - b).abs() < 2.0 / 65535.0));
        }
    }
    assert_eq!(std::fs::read(recipe).unwrap(), saved);
}

#[test]
fn single_developed_panel_needs_no_camera_and_matches_render() {
    let f = Fixture::new("single-recipe");
    let raw = f.raw(1, None);
    let recipe = f.recipe("alternate.json", json!({"exposure": 0.3, "crop": {"top": 0.2}}));
    let panel = format!("recipe:{}", recipe.display());
    let output = f.path("single.png");
    let render = f.path("render.png");
    run(&["compare", s(&raw), "--panel", &panel, "-o", s(&output)], true);
    run(&["render", s(&raw), "--recipe", s(&recipe), "-o", s(&render)], true);
    assert_eq!(std::fs::read(output).unwrap(), std::fs::read(render).unwrap());
}

#[test]
fn camera_matches_saved_crop_and_rotation_without_changing_recipes() {
    let f = Fixture::new("framing");
    let embedded = jpeg(96, 64, true);
    let camera = decoded_jpeg(&embedded);
    let recipe = f.recipe(
        "frame.ARW.rawsmith.json",
        json!({
            "rotate": 7, "crop": {"left": 0.25, "top": 0.125, "right": 0.875, "bottom": 0.75}
        }),
    );
    let original = std::fs::read(&recipe).unwrap();
    for orientation in [1, 3, 6, 8] {
        let raw = f.raw(orientation, Some(&embedded));
        let output = f.path("comparison.png");
        let report = run(&["compare", s(&raw), "-o", s(&output)], true);
        assert_eq!(report["framing_matched"], true);
        assert_eq!(report["panels"][1]["recipe_file"], s(&recipe));
        assert_eq!(report["panels"][0]["framing"], report["panels"][1]["framing"]);
        assert_eq!(report["panels"][0]["width"], report["panels"][1]["width"]);
        assert_eq!(report["panels"][0]["height"], report["panels"][1]["height"]);
        let (image, _) = png(&output);
        // Independently map a cropped/rotated output pixel back to stored JPEG
        // coordinates, including the RAW orientation (JPEG metadata is ignored).
        let (w, h) = if matches!(orientation, 6 | 8) { (64f32, 96f32) } else { (96f32, 64f32) };
        let (sin, cos) = 7f32.to_radians().sin_cos();
        let keep = w.min(h) / (w.max(h) * sin + w.min(h) * cos);
        let (rw, rh) = ((w * keep) as usize, (h * keep) as usize);
        let (left, top) = ((rw as f64 * 0.25).round() as usize, (rh as f64 * 0.125).round() as usize);
        let pw = (rw as f64 * 0.875).round() as usize - left;
        let ph = (rh as f64 * 0.75).round() as usize - top;
        assert_eq!(report["panels"][0]["width"], pw);
        assert_eq!(report["panels"][0]["height"], ph);
        let (x, y) = (pw / 3, ph / 3);
        let dx = (left + x) as f32 - (rw as f32 - 1.0) * 0.5;
        let dy = (top + y) as f32 - (rh as f32 - 1.0) * 0.5;
        let ux = (w - 1.0) * 0.5 + dx * cos + dy * sin;
        let uy = (h - 1.0) * 0.5 - dx * sin + dy * cos;
        let (sx, sy) = match orientation {
            3 => (95.0 - ux, 63.0 - uy),
            6 => (uy, 63.0 - ux),
            8 => (95.0 - uy, ux),
            _ => (ux, uy),
        };
        for (got, want) in image.at(x, y).into_iter().zip(camera.sample(sx, sy)) {
            assert!((got - want).abs() < 2.0 / 65535.0, "{got} vs {want}");
        }
    }
    assert_eq!(std::fs::read(recipe).unwrap(), original);
}

#[test]
fn chosen_panel_order_mismatched_framing_sizes_and_determinism() {
    let f = Fixture::new("versions");
    let raw = f.raw(6, Some(&jpeg(192, 128, true)));
    let saved = f.recipe("frame.ARW.rawsmith.json", json!({"crop": {"top": 0.25, "bottom": 0.75, "aspect": "free"}}));
    let old = f.recipe("earlier.json", json!({"exposure": -0.5}));
    let original = [std::fs::read(&raw).unwrap(), std::fs::read(&saved).unwrap(), std::fs::read(&old).unwrap()];
    let explicit = format!("recipe:{}", old.display());
    let output = f.path("comparison.jpg");
    let args = [
        "compare",
        s(&raw),
        "-o",
        s(&output),
        "--size",
        "80",
        "--panel",
        "saved",
        "--panel",
        "camera",
        "--panel",
        &explicit,
        "--panel",
        "default",
    ];
    let mut report = run(&args, true);
    assert_eq!(report["camera_framing_panel"], 0);
    assert_eq!(report["framing_matched"], false);
    assert_eq!(report["warnings"].as_array().unwrap().len(), 1);
    let panels = report["panels"].as_array().unwrap();
    assert_eq!(
        panels.iter().map(|p| p["kind"].as_str().unwrap()).collect::<Vec<_>>(),
        ["saved", "camera", "recipe", "default"]
    );
    assert_eq!(panels[0]["recipe_file"], s(&saved));
    assert_eq!(panels[1]["recipe_file"], Value::Null);
    assert_eq!(panels[2]["recipe_file"], s(&old));
    assert_eq!(panels[3]["recipe_file"], Value::Null);
    for (panel, (w, h)) in panels.iter().zip([(64, 48), (64, 48), (32, 48), (32, 48)]) {
        assert_eq!(panel["width"], w);
        assert_eq!(panel["height"], h);
    }
    let bytes = std::fs::read(&output).unwrap();
    let mut again = run(&args, true);
    assert_eq!(std::fs::read(&output).unwrap(), bytes);
    assert_eq!(again["output_replaced"], true);
    report["output_replaced"] = Value::Null;
    again["output_replaced"] = Value::Null;
    assert_eq!(report, again);
    for (path, want) in [&raw, &saved, &old].into_iter().zip(original) {
        assert_eq!(std::fs::read(path).unwrap(), want);
    }
    let tiny = run(&["compare", s(&raw), "-o", s(&output), "--size", "1"], true);
    assert!(tiny["panels"].as_array().unwrap().iter().all(|p| p["width"] == 1 && p["height"] == 1));
}

fn with_exif(jpeg: &[u8], space: u16, interop: &[u8; 4]) -> Vec<u8> {
    let mut data = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
    data.extend(1u16.to_le_bytes());
    data.extend([0x69, 0x87, 4, 0, 1, 0, 0, 0, 26, 0, 0, 0]);
    data.extend(0u32.to_le_bytes());
    data.extend(2u16.to_le_bytes());
    data.extend([1, 0xa0, 3, 0, 1, 0, 0, 0]);
    data.extend(space.to_le_bytes());
    data.extend([0, 0]);
    data.extend([5, 0xa0, 4, 0, 1, 0, 0, 0, 56, 0, 0, 0]);
    data.extend(0u32.to_le_bytes());
    data.extend(1u16.to_le_bytes());
    data.extend([1, 0, 2, 0, 4, 0, 0, 0]);
    data.extend(interop);
    data.extend(0u32.to_le_bytes());
    let mut out = jpeg[..2].to_vec();
    out.extend([0xff, 0xe1]);
    out.extend(((data.len() + 2) as u16).to_be_bytes());
    out.extend(data);
    out.extend(&jpeg[2..]);
    out
}

#[test]
fn camera_colour_declarations_control_warnings_and_output_tagging() {
    let f = Fixture::new("colour");
    let untagged = jpeg(96, 64, false);
    let mut unfamiliar_icc = jpeg(96, 64, true);
    let profile = include_bytes!("../assets/profiles/sRGB-v2-magic.icc");
    let start = unfamiliar_icc.windows(profile.len()).position(|p| p == profile).unwrap();
    unfamiliar_icc[start + 80] ^= 1;
    let mut broken_icc = jpeg(96, 64, true);
    let icc_marker = broken_icc.windows(12).position(|p| p == b"ICC_PROFILE\0").unwrap();
    broken_icc[icc_marker + 12] = 0; // Invalid ICC chunk sequence, despite EXIF sRGB.
    for (embedded, space, srgb) in [
        (with_exif(&untagged, 1, b"R98\0"), "sRGB", true),
        (with_exif(&untagged, 65535, b"R03\0"), "sRGB", true),
        (with_exif(&untagged, 65535, b"\0\0\0\0"), "uncalibrated", false),
        (with_exif(&untagged, 1, b"R03\0"), "unknown", false),
        (untagged, "unknown", false),
        (with_exif(&unfamiliar_icc, 1, b"R98\0"), "ICC (unverified)", false),
        (with_exif(&broken_icc, 1, b"R98\0"), "ICC (unverified)", false),
    ] {
        let raw = f.raw(1, Some(&embedded));
        for extension in ["jpg", "png"] {
            let output = f.path(&format!("comparison.{extension}"));
            let report = run(&["compare", s(&raw), "-o", s(&output)], true);
            assert_eq!(report["panels"][0]["color_space"], space);
            assert_eq!(report["output_color_space"], if srgb { "sRGB" } else { "mixed-unmanaged" });
            assert_eq!(report["warnings"].as_array().unwrap().is_empty(), srgb);
            if extension == "png" {
                assert_eq!(png(&output).1, srgb);
            } else {
                let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(std::fs::read(output).unwrap()));
                decoder.decode().unwrap();
                assert_eq!(decoder.icc_profile().is_some(), srgb);
                assert!(decoder.exif_data().is_none());
            }
        }
    }
}

#[test]
fn bare_camera_jpeg_uses_raw_exif_and_reports_conversion() {
    let f = Fixture::new("raw-colour");
    let embedded = jpeg(96, 64, false);
    let original = decoded_jpeg(&embedded);
    for (space, interop, declared, converted, srgb) in [
        (Some(1), Some(*b"R98\0"), "sRGB", false, true),
        (Some(65535), Some(*b"R98\0"), "sRGB", false, true),
        (Some(1), None, "sRGB", false, true),
        (None, Some(*b"R98\0"), "sRGB", false, true),
        (Some(65535), Some(*b"R03\0"), "Adobe RGB (1998)", true, true),
        (None, Some(*b"R03\0"), "Adobe RGB (1998)", true, true),
        (None, None, "unknown", false, false),
        (Some(65535), None, "uncalibrated", false, false),
        (Some(1), Some(*b"R03\0"), "unknown", false, false),
    ] {
        // The declaration is in the RAW's top-level EXIF, never in its sensor
        // SubIFD or its bare JPEG. Test an upright and orientation-8 frame.
        for orientation in [1, 8] {
            let raw = f.raw_with_colour(orientation, Some(&embedded), space, interop);
            for extension in ["jpg", "png"] {
                let output = f.path(&format!("comparison.{extension}"));
                let report = run(&["compare", s(&raw), "--panel", "camera", "-o", s(&output)], true);
                assert_eq!(report["panels"].as_array().unwrap().len(), 1);
                let panel = &report["panels"][0];
                assert_eq!(
                    panel["color_declaration"],
                    json!({
                        "source": if space.is_some() || interop.is_some() { "raw-exif" } else { "none" },
                        "color_space": declared,
                    })
                );
                assert_eq!(panel["color_space"], if srgb { "sRGB" } else { declared });
                assert_eq!(
                    panel["color_conversion"],
                    if converted { json!("Adobe RGB (1998) to sRGB") } else { Value::Null }
                );
                assert_eq!(report["warnings"].as_array().unwrap().is_empty(), srgb);
                assert_eq!(report["output_color_space"], if srgb { "sRGB" } else { "mixed-unmanaged" });
                if extension == "png" {
                    let (image, tagged) = png(&output);
                    assert_eq!(tagged, srgb);
                    // Unconverted sRGB/unknown pixels must survive orientation
                    // and PNG encoding without an implicit colour transform.
                    if !converted {
                        for y in 0..panel["height"].as_u64().unwrap() as usize {
                            for x in 0..panel["width"].as_u64().unwrap() as usize {
                                let want = if orientation == 8 { original.at(95 - y, x) } else { original.at(x, y) };
                                assert!(image.at(x, y).iter().zip(want).all(|(a, b)| (a - b).abs() < 2.0 / 65535.0));
                            }
                        }
                    }
                } else {
                    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(std::fs::read(output).unwrap()));
                    decoder.decode().unwrap();
                    assert_eq!(decoder.icc_profile().is_some(), srgb);
                    assert!(decoder.exif_data().is_none());
                }
            }
        }
    }
}

#[test]
fn jpeg_colour_declaration_takes_precedence_over_raw_exif() {
    let f = Fixture::new("colour-precedence");
    let bare = jpeg(96, 64, false);
    let mut unfamiliar_icc = jpeg(96, 64, true);
    let profile = include_bytes!("../assets/profiles/sRGB-v2-magic.icc");
    let start = unfamiliar_icc.windows(profile.len()).position(|p| p == profile).unwrap();
    unfamiliar_icc[start + 80] ^= 1;
    for (embedded, raw_space, raw_index, source, declared, converted, srgb) in [
        (jpeg(96, 64, true), 65535, *b"R03\0", "embedded-jpeg-icc", "sRGB", false, true),
        (with_exif(&bare, 1, b"R98\0"), 65535, *b"R03\0", "embedded-jpeg-exif", "sRGB", false, true),
        (with_exif(&bare, 65535, b"R03\0"), 1, *b"R98\0", "embedded-jpeg-exif", "Adobe RGB (1998)", true, true),
        (with_exif(&bare, 65535, b"\0\0\0\0"), 1, *b"R98\0", "embedded-jpeg-exif", "uncalibrated", false, false),
        (with_exif(&bare, 1, b"R03\0"), 1, *b"R98\0", "embedded-jpeg-exif", "unknown", false, false),
        (unfamiliar_icc, 1, *b"R98\0", "embedded-jpeg-icc", "ICC (unverified)", false, false),
    ] {
        let raw = f.raw_with_colour(1, Some(&embedded), Some(raw_space), Some(raw_index));
        let output = f.path("comparison.png");
        let report = run(&["compare", s(&raw), "-o", s(&output)], true);
        let panel = &report["panels"][0];
        assert_eq!(panel["color_declaration"], json!({"source": source, "color_space": declared}));
        assert_eq!(panel["color_conversion"].is_string(), converted);
        assert_eq!(report["output_color_space"], if srgb { "sRGB" } else { "mixed-unmanaged" });
        assert_eq!(png(&output).1, srgb);
    }
}

#[test]
fn embedded_orientation_is_ignored_and_camera_metadata_is_removed() {
    let f = Fixture::new("jpeg-orientation");
    let original = jpeg(96, 64, true);
    // TIFF IFD0 Orientation=8 deliberately contradicts the RAW's Orientation=1.
    let mut exif = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
    exif.extend(1u16.to_le_bytes());
    exif.extend([0x12, 1, 3, 0, 1, 0, 0, 0, 8, 0, 0, 0]);
    exif.extend(0u32.to_le_bytes());
    let mut embedded = original[..2].to_vec();
    embedded.extend([0xff, 0xe1]);
    embedded.extend(((exif.len() + 2) as u16).to_be_bytes());
    embedded.extend(exif);
    embedded.extend(&original[2..]);
    let raw = f.raw(1, Some(&embedded));
    let output = f.path("comparison.jpg");
    let report = run(&["compare", s(&raw), "-o", s(&output)], true);
    assert_eq!(report["panels"][0]["width"], 96);
    assert_eq!(report["panels"][0]["height"], 64);
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(std::fs::read(output).unwrap()));
    let bytes = decoder.decode().unwrap();
    assert!(decoder.exif_data().is_none());
    let camera = decoded_jpeg(&original);
    for (got, want) in bytes[..3].iter().zip(camera.at(0, 0)) {
        assert!((*got as f32 / 255.0 - want).abs() < 0.025);
    }
}

#[cfg(unix)]
#[test]
fn output_aliases_cannot_replace_a_raw_or_recipe() {
    let f = Fixture::new("aliases");
    let raw = f.raw(1, Some(&jpeg(96, 64, true)));
    let saved = f.recipe("frame.ARW.rawsmith.json", json!({"exposure": 0.2}));
    let old = f.recipe("old.json", json!({"contrast": 5}));
    let explicit = format!("recipe:{}", old.display());
    for (index, source) in [&raw, &saved, &old].into_iter().enumerate() {
        let bytes = std::fs::read(source).unwrap();
        for symlink in [false, true] {
            let output = f.path(&format!("alias-{index}-{symlink}.jpg"));
            if symlink {
                std::os::unix::fs::symlink(source, &output).unwrap();
            } else {
                std::fs::hard_link(source, &output).unwrap();
            }
            let error = run(&["compare", s(&raw), "-o", s(&output), "--panel", "camera", "--panel", &explicit], false);
            assert_eq!(error["error"]["kind"], "params");
            assert_eq!(std::fs::read(source).unwrap(), bytes);
            assert_eq!(std::fs::read(output).unwrap(), bytes);
        }
    }
    let output = f.path("dangling.jpg");
    std::os::unix::fs::symlink("missing.jpg", &output).unwrap();
    assert_eq!(run(&["compare", s(&raw), "-o", s(&output)], true)["output_replaced"], true);
    assert!(!f.path("missing.jpg").exists());
}

#[test]
fn all_failures_preserve_raw_recipes_and_existing_outputs() {
    let f = Fixture::new("errors");
    let raw = f.raw(1, Some(&jpeg(96, 64, true)));
    let saved = f.recipe("frame.ARW.rawsmith.json", json!({"exposure": 0.2}));
    let invalid = f.recipe("invalid.json", json!({"exposure": 99}));
    let output = f.path("comparison.jpg");
    std::fs::write(&output, b"existing comparison").unwrap();
    let invalid_panel = format!("recipe:{}", invalid.display());
    let missing_panel = format!("recipe:{}", f.path("missing.json").display());
    let original = [std::fs::read(&raw).unwrap(), std::fs::read(&saved).unwrap(), std::fs::read(&invalid).unwrap()];
    for extra in [
        vec!["--size", "0"],
        vec!["--size", "bad"],
        vec!["--panel", "camera", "--panel", "nope"],
        vec!["--panel", "camera", "--panel", &invalid_panel],
        vec!["--panel", "camera", "--panel", &missing_panel],
    ] {
        let mut args = vec!["compare", s(&raw), "-o", s(&output)];
        args.extend(extra);
        assert!(run(&args, false)["error"]["kind"].is_string());
        assert_eq!(std::fs::read(&output).unwrap(), b"existing comparison");
    }
    for destination in [&raw, &saved, &invalid] {
        run(&["compare", s(&raw), "-o", s(destination), "--panel", "camera", "--panel", &invalid_panel], false);
    }
    for (path, bytes) in [&raw, &saved, &invalid].into_iter().zip(&original) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
    for (orientation, jpeg) in [(2, Some(jpeg(96, 64, true))), (1, None), (1, Some(b"damaged JPEG".to_vec()))] {
        let raw = f.raw(orientation, jpeg.as_deref());
        let original = std::fs::read(&raw).unwrap();
        run(&["compare", s(&raw), "-o", s(&output)], false);
        assert_eq!(std::fs::read(raw).unwrap(), original);
        assert_eq!(std::fs::read(&output).unwrap(), b"existing comparison");
    }
    let raw = f.raw(1, None);
    // No camera is needed to compare developed recipes.
    run(&["compare", s(&raw), "-o", s(&output), "--panel", "saved", "--panel", "default"], true);
    for path in [f.path("missing/comparison.jpg"), f.path("directory.jpg"), f.path("comparison.bmp")] {
        if path.ends_with("directory.jpg") {
            std::fs::create_dir(&path).unwrap();
        } else if path.ends_with("comparison.bmp") {
            std::fs::write(&path, b"old unsupported image").unwrap();
        }
        run(&["compare", s(&raw), "-o", s(&path), "--panel", "default", "--panel", "saved"], false);
        if path.ends_with("comparison.bmp") {
            assert_eq!(std::fs::read(path).unwrap(), b"old unsupported image");
        }
    }
    assert_eq!(std::fs::read(saved).unwrap(), original[1]);
    assert!(std::fs::read_dir(&f.0).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[test]
fn comparison_help_describes_panel_and_colour_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_rawsmith")).args(["compare", "--help"]).output().unwrap();
    assert!(output.status.success() && output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    for text in [
        "camera, saved, default",
        "single --panel",
        "recipe:FILE",
        "1400",
        "--size",
        "each panel's longest edge",
        "crop and rotation",
        "untagged",
        "RAW EXIF",
        "Adobe RGB (1998)",
        "converted to sRGB",
        "only read",
        "output_replaced",
    ] {
        assert!(help.contains(text), "{help}");
    }
}

#[test]
fn compare_honours_and_reports_jpeg_quality() {
    let f = Fixture::new("quality");
    let raw = f.raw(1, None);
    let image_path = f.path("row.jpg");
    let mut previous = Vec::new();
    for quality in ["92", "85", "1", "100"] {
        let report = run(&["compare", s(&raw), "--panel", "default", "-o", s(&image_path), "--quality", quality], true);
        let bytes = std::fs::read(&image_path).unwrap();
        assert_eq!(report["quality"], quality.parse::<u8>().unwrap());
        assert_eq!(report["output_bytes"], bytes.len());
        assert_ne!(bytes, previous);
        let file = std::fs::read(&raw).unwrap();
        let decoded = rawsmith::arw::decode(&file).unwrap();
        let panels = [rawsmith::compare::Panel {
            kind: "default",
            params: rawsmith::params::Params::default(),
            recipe_file: None,
        }];
        let (image, _, srgb) = rawsmith::compare::row(&decoded, &file, &panels, 1400).unwrap();
        assert_eq!(
            bytes,
            output::encode_with_color_space(&image_path, &image, quality.parse().unwrap(), srgb).unwrap()
        );
        previous = bytes;
    }
    for quality in ["0", "101"] {
        run(&["compare", s(&raw), "--panel", "default", "-o", s(&image_path), "--quality", quality], false);
        assert_eq!(std::fs::read(&image_path).unwrap(), previous);
    }
    let png = f.path("row.png");
    let report = run(&["compare", s(&raw), "--panel", "default", "-o", s(&png)], true);
    assert_eq!(report.get("quality"), Some(&Value::Null));
}

#[test]
fn recipe_option_names_the_saved_panel_like_the_other_commands() {
    let f = Fixture::new("recipe-option");
    let raw = f.raw(1, Some(&jpeg(96, 64, true)));
    let sidecar = f.recipe("frame.ARW.rawsmith.json", json!({"exposure": 1.0}));
    let chosen = f.recipe("chosen.json", json!({"crop": {"top": 0.25, "bottom": 0.75, "aspect": "free"}}));
    let originals = [std::fs::read(&sidecar).unwrap(), std::fs::read(&chosen).unwrap()];
    let output = f.path("comparison.jpg");
    let report = run(&["compare", s(&raw), "--recipe", s(&chosen), "-o", s(&output)], true);
    let panels = report["panels"].as_array().unwrap();
    assert_eq!(panels.iter().map(|p| p["kind"].as_str().unwrap()).collect::<Vec<_>>(), ["camera", "saved"]);
    assert_eq!(panels[1]["recipe_file"], s(&chosen));
    assert_eq!(panels[1]["framing"], panels[0]["framing"]);
    assert_eq!(report["framing_matched"], true);
    // `saved` in an explicit panel list means the same file.
    let report = run(
        &["compare", s(&raw), "--recipe", s(&chosen), "--panel", "saved", "--panel", "default", "-o", s(&output)],
        true,
    );
    assert_eq!(report["panels"][0]["recipe_file"], s(&chosen));

    // A named recipe that does not exist is an error, not the defaults.
    let missing = f.path("missing.json");
    let before = std::fs::read(&output).unwrap();
    let error = run(&["compare", s(&raw), "--recipe", s(&missing), "-o", s(&output)], false);
    assert_eq!(error["error"]["kind"], "io");
    assert!(!missing.exists());
    // Also when no panel shows the saved recipe.
    let error = run(&["compare", s(&raw), "--recipe", s(&missing), "--panel", "default", "-o", s(&output)], false);
    assert_eq!(error["error"]["kind"], "io");
    // The chosen recipe is protected from being used as the output.
    let error = run(&["compare", s(&raw), "--recipe", s(&chosen), "-o", s(&chosen)], false);
    assert_eq!(error["error"]["kind"], "params");
    assert_eq!(std::fs::read(&output).unwrap(), before);
    assert_eq!([std::fs::read(&sidecar).unwrap(), std::fs::read(&chosen).unwrap()], originals);
}

#[test]
fn region_shows_the_same_part_of_every_panel() {
    let f = Fixture::new("region");
    let embedded = jpeg(96, 64, true);
    let camera = decoded_jpeg(&embedded);
    let raw = f.raw(1, Some(&embedded));
    let output = f.path("region.png");
    let report = run(
        &[
            "compare",
            s(&raw),
            "--panel",
            "camera",
            "--panel",
            "default",
            "--region",
            "0.5,0.25,1,0.75",
            "-o",
            s(&output),
        ],
        true,
    );
    assert_eq!(report["region"], json!([0.5, 0.25, 1.0, 0.75]));
    for panel in report["panels"].as_array().unwrap() {
        assert_eq!((panel["width"].as_u64(), panel["height"].as_u64()), (Some(48), Some(32)));
    }
    // The camera panel is the native region, unscaled.
    let (image, _) = png(&output);
    for (got, want) in image.at(5, 7).into_iter().zip(camera.at(48 + 5, 16 + 7)) {
        assert!((got - want).abs() < 2.0 / 65535.0, "{got} vs {want}");
    }
    // Rounding that differs between endpoints must not resample the panel.
    let odd = run(&["compare", s(&raw), "--panel", "default", "--region", "0.25,0.4,0.75,0.9", "-o", s(&output)], true);
    assert_eq!((odd["panels"][0]["width"].as_u64(), odd["panels"][0]["height"].as_u64()), (Some(48), Some(31)));
    let error = run(&["compare", s(&raw), "--region", "0.5,0.5,0.4,1", "-o", s(&f.path("bad.jpg"))], false);
    assert_eq!(error["error"]["kind"], "params");
    assert!(!f.path("bad.jpg").exists());
}

#[test]
fn brief_keeps_losses_and_drops_looks() {
    let f = Fixture::new("brief");
    let raw = f.raw(1, Some(&jpeg(96, 64, true)));
    let output = f.path("brief.jpg");
    let report = run(&["compare", s(&raw), "--brief", "-o", s(&output)], true);
    let panels = report["panels"].as_array().unwrap();
    assert!(panels.iter().all(|p| p.get("look").is_none() && p["kind"].is_string()));
    assert!(panels[1]["losses_against_camera"].is_array());
    assert!(panels[0].get("losses_against_camera").is_none());
    assert_eq!(report["output"], s(&output));
    // An older recipe keeps its compatibility note in the brief report.
    let old = f.recipe("old.json", json!({"rawsmith": "0.1.0", "exposure": 0.2}));
    let report = run(&["compare", s(&raw), "--recipe", s(&old), "--brief", "-o", s(&output)], true);
    assert!(report["panels"][1]["recipe_compatibility"]["note"].is_string(), "{report}");
    let current = f.recipe("new.json", json!({"rawsmith": env!("CARGO_PKG_VERSION"), "exposure": 0.2}));
    let report = run(&["compare", s(&raw), "--recipe", s(&current), "--brief", "-o", s(&output)], true);
    assert!(report["panels"][1].get("recipe_compatibility").is_none());
}

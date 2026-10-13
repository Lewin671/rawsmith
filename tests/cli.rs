//! End-to-end tests of the command-line interface on synthetic RAW files.

use std::path::{Path, PathBuf};
use std::process::Command;

use rawsmith::synthetic::Synthetic;
use serde_json::Value;

/// A scratch directory holding one synthetic RAW: a grey ramp with a
/// saturated-colour band and a blown highlight patch.
struct Fixture {
    dir: PathBuf,
    raw: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rawsmith-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (320, 240);
        let file = Synthetic::from_camera_rgb(w, h, |x, y| {
            let ramp = x as f32 / w as f32 * 0.35;
            // Camera RGB before white balance: neutral is (1/2.54, 1, 1/1.56) of grey.
            let neutral = [ramp / 2.54, ramp, ramp / 1.56];
            if y > h * 3 / 4 {
                [neutral[0] * 2.0, neutral[1] * 0.6, neutral[2] * 0.3]
            } else if x > w * 7 / 8 && y < h / 4 {
                [1.0, 1.0, 1.0]
            } else {
                neutral
            }
        });
        let raw = dir.join("frame.ARW");
        std::fs::write(&raw, file.to_bytes()).unwrap();
        Fixture { dir, raw }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn recipe(&self) -> PathBuf {
        self.dir.join("frame.ARW.rawsmith.json")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run(args: &[&str]) -> (bool, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_rawsmith")).args(args).output().unwrap();
    if out.status.success() {
        assert!(out.stderr.is_empty(), "successful commands must leave stderr empty");
    } else {
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "failed commands must leave stdout empty: {args:?}");
    }
    let text = if out.status.success() { &out.stdout } else { &out.stderr };
    let value = serde_json::from_slice(text)
        .unwrap_or_else(|_| panic!("output is not JSON: {}", String::from_utf8_lossy(text)));
    (out.status.success(), value)
}

fn ok(args: &[&str]) -> Value {
    let (success, value) = run(args);
    assert!(success, "command failed: {args:?} -> {value}");
    value
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_return_json_before_writing_and_preserve_existing_files() {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;

    let f = Fixture::new("non-utf8-paths");
    let image = rawsmith::image::Image::new(64, 48);
    let jpeg = rawsmith::output::encode(Path::new("camera.jpg"), &image, 92).unwrap();
    let mut raw = std::fs::read(&f.raw).unwrap();
    embed_jpeg(&mut raw, &jpeg);
    std::fs::write(&f.raw, &raw).unwrap();
    let bad_raw = f.dir.join(OsString::from_vec(b"raw-\xff.ARW".to_vec()));
    let bad_recipe = f.dir.join(OsString::from_vec(b"recipe-\xff.json".to_vec()));
    let bad_output = f.dir.join(OsString::from_vec(b"output-\xff.jpg".to_vec()));
    let mut sidecar = bad_raw.clone().into_os_string();
    sidecar.push(".rawsmith.json");
    let bad_sidecar = PathBuf::from(sidecar);
    let preview = f.path("preview.jpg");
    std::fs::write(f.recipe(), b"{\"exposure\":0.5}").unwrap();
    std::fs::write(&preview, b"previous preview").unwrap();
    let paths = vec![f.raw.clone(), f.recipe(), preview.clone()];
    // macOS filesystems reject non-UTF-8 names themselves. Still exercise
    // every invalid argument there and verify the existing UTF-8 companions.
    // On other Unix systems also cover replacing existing non-UTF-8 files.
    #[cfg(not(target_os = "macos"))]
    let paths = {
        let mut paths = paths;
        for path in [&bad_recipe, &bad_sidecar] {
            std::fs::write(path, b"{\"exposure\":0.5}").unwrap();
        }
        std::fs::write(&bad_raw, &raw).unwrap();
        std::fs::write(&bad_output, b"previous preview").unwrap();
        paths.extend([bad_raw.clone(), bad_recipe.clone(), bad_sidecar, bad_output.clone()]);
        paths
    };
    #[cfg(target_os = "macos")]
    let _ = bad_sidecar;
    let before: Vec<_> = paths
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let check = |args: &[&OsStr], invalid: &Path| {
        let out = Command::new(env!("CARGO_BIN_EXE_rawsmith")).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let error: Value = serde_json::from_slice(&out.stderr)
            .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&out.stderr)));
        assert_eq!(error["error"]["kind"], "params", "{args:?}: {error}");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("UTF-8") && message.contains(invalid.to_string_lossy().as_ref()), "{error}");
        for (path, bytes) in &before {
            assert_eq!(std::fs::read(path).unwrap(), *bytes, "{args:?}: {path:?}");
        }
        assert_eq!(std::fs::read_dir(&f.dir).unwrap().count(), before.len(), "no staging files or new outputs");
    };
    for command in ["inspect", "render", "edit", "auto", "extract-jpeg", "compare"] {
        let mut args = vec![OsStr::new(command), bad_raw.as_os_str()];
        if command != "inspect" {
            args.extend([OsStr::new("-o"), preview.as_os_str()]);
        }
        check(&args, &bad_raw);
        if command == "inspect" {
            continue;
        }
        check(&[OsStr::new(command), f.raw.as_os_str(), OsStr::new("-o"), bad_output.as_os_str()], &bad_output);
        if matches!(command, "render" | "edit" | "auto") {
            for output in [false, true] {
                let mut args =
                    vec![OsStr::new(command), f.raw.as_os_str(), OsStr::new("--recipe"), bad_recipe.as_os_str()];
                if output {
                    args.extend([OsStr::new("-o"), preview.as_os_str()]);
                }
                check(&args, &bad_recipe);
            }
        }
    }
    let mut panel = OsString::from("recipe:");
    panel.push(&bad_recipe);
    check(
        &[
            OsStr::new("compare"),
            f.raw.as_os_str(),
            OsStr::new("-o"),
            preview.as_os_str(),
            OsStr::new("--panel"),
            &panel,
        ],
        &bad_recipe,
    );
}

#[test]
fn inspect_reports_camera_and_exposure() {
    let f = Fixture::new("inspect");
    let v = ok(&["inspect", s(&f.raw)]);
    assert_eq!(v["camera"]["model"], "ILCE-7M4");
    assert_eq!(v["compression"], "uncompressed");
    assert_eq!(v["sensor"]["width"], 320);
    assert!(v["exposure"]["saturated_pct"][1].as_f64().unwrap() > 1.0);
    let kelvin = v["white_balance"]["temperature"].as_f64().unwrap();
    assert!((3000.0..9000.0).contains(&kelvin), "{kelvin}");
}

#[test]
fn render_is_deterministic_and_leaves_the_raw_untouched() {
    let f = Fixture::new("render");
    let before = std::fs::read(&f.raw).unwrap();
    let (a, b) = (f.path("a.png"), f.path("b.png"));
    let report = ok(&["render", s(&f.raw), "-o", s(&a)]);
    ok(&["render", s(&f.raw), "-o", s(&b)]);
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    assert_eq!(std::fs::read(&f.raw).unwrap(), before);
    assert!(!f.recipe().exists(), "render must not create a recipe");
    assert_eq!(report["stats"]["width"], 320);
    // The grey ramp stays neutral through white balance and the colour matrix.
    let cast = &report["stats"]["neutral_cast"];
    assert!(cast[0].as_f64().unwrap().abs() < 1.0 && cast[1].as_f64().unwrap().abs() < 1.0, "{cast}");
}

#[test]
fn edits_change_the_picture_in_the_expected_direction() {
    let f = Fixture::new("direction");
    let out = f.path("o.jpg");
    let render = |set: &[&str]| {
        let mut args = vec!["render", s(&f.raw), "-o", s(&out), "--size", "160"];
        for a in set {
            args.extend(["--set", a]);
        }
        ok(&args)["stats"].clone()
    };
    let tone = |v: &Value, key: &str| v["tones"][key].as_f64().unwrap();
    let base = render(&[]);
    assert_eq!(base["width"], 160);
    assert!(tone(&render(&["exposure=1"]), "p50") > tone(&base, "p50") + 5.0);
    assert!(tone(&render(&["exposure=-1"]), "p50") < tone(&base, "p50") - 5.0);
    assert!(tone(&render(&["contrast=60"]), "spread") > tone(&base, "spread"));
    assert!(tone(&render(&["shadows=80"]), "p5") > tone(&base, "p5"));
    assert!(tone(&render(&["highlights=-80"]), "p95") < tone(&base, "p95"));
    assert!(tone(&render(&["blacks=-60"]), "p5") < tone(&base, "p5"));
    assert!(tone(&render(&["whites=60"]), "p75") > tone(&base, "p75") + 2.0);
    assert!(tone(&render(&["whites=-60"]), "p75") < tone(&base, "p75") - 2.0);
    // A graduated filter darkens the side it starts from and leaves the far side alone.
    let zone = |v: &Value, row: usize, column: usize| v["zones"][row][column].as_f64().unwrap();
    let graded = render(&[r#"gradients=[{"from":[0,0.5],"to":[0.5,0.5],"exposure":-1.5}]"#]);
    assert!(zone(&graded, 1, 0) < zone(&base, 1, 0) - 3.0, "{graded}");
    assert!((zone(&graded, 1, 2) - zone(&base, 1, 2)).abs() < 0.5);
    let corners = render(&["vignette=80"]);
    assert!(zone(&corners, 0, 0) > zone(&base, 0, 0) && (zone(&corners, 1, 1) - zone(&base, 1, 1)).abs() < 1.5);
    let chroma = |v: &Value| v["chroma_mean"].as_f64().unwrap();
    assert!(chroma(&render(&["saturation=50"])) > chroma(&base));
    assert!(chroma(&render(&["saturation=-100"])) < 0.5);
    // Warmer white balance pushes neutrals towards yellow (+b).
    let warm = render(&["temperature=9000"]);
    assert!(
        warm["neutral_cast"][1].as_f64().unwrap() > base["neutral_cast"][1].as_f64().unwrap() + 1.0
            || chroma(&warm) > chroma(&base)
    );
}

#[test]
fn edit_saves_a_sparse_recipe_that_render_then_uses() {
    let f = Fixture::new("edit");
    let v = ok(&["edit", s(&f.raw), "--set", "exposure=0.5", "--set", "hsl.orange.saturation=20"]);
    assert_eq!(v["recipe"]["exposure"], 0.5);
    let saved: Value = serde_json::from_slice(&std::fs::read(f.recipe()).unwrap()).unwrap();
    assert_eq!(saved["exposure"], 0.5);
    assert_eq!(saved["hsl"]["orange"]["saturation"], 20.0);
    assert!(saved.get("contrast").is_none(), "defaults are not stored");
    assert!(saved["rawsmith"].is_string());

    // += changes the saved value; the saved recipe is what render uses.
    let v = ok(&["edit", s(&f.raw), "--set", "exposure+=0.25", "-o", s(&f.path("p.jpg")), "--size", "160"]);
    assert_eq!(v["recipe"]["exposure"], 0.75);
    assert!(f.path("p.jpg").exists());
    assert_eq!(ok(&["render", s(&f.raw)])["recipe"]["exposure"], 0.75);
    assert_eq!(ok(&["edit", s(&f.raw), "--reset"])["recipe"], serde_json::json!({}));
}

#[test]
fn auto_writes_an_explained_recipe() {
    let f = Fixture::new("auto");
    let v = ok(&["auto", s(&f.raw), "-o", s(&f.path("auto.jpg")), "--size", "160"]);
    assert!(v["decisions"].as_array().unwrap().len() >= 2);
    assert!(f.recipe().exists() && f.path("auto.jpg").exists());
    assert!(v["stats"]["tones"]["p50"].as_f64().unwrap() > 20.0);
}

#[test]
fn region_renders_full_resolution_detail() {
    let f = Fixture::new("region");
    let inspect = ok(&["inspect", s(&f.raw)]);
    let v = ok(&["render", s(&f.raw), "--region", "0.25,0.25,0.75,0.75", "-o", s(&f.path("r.png"))]);
    assert_eq!((v["stats"]["width"].as_u64(), v["stats"]["height"].as_u64()), (Some(160), Some(120)));
    assert_eq!(v["region"], serde_json::json!([0.25, 0.25, 0.75, 0.75]));
    assert_eq!(v["sensor_clipped_scope"], "full_active_area");
    assert_eq!(v["sensor_clipped_pct"], inspect["exposure"]["sensor_clipped_pct"]);
    let full = ok(&["render", s(&f.raw)]);
    assert_eq!(full["region"], Value::Null);
    assert_eq!(full["sensor_clipped_pct"], v["sensor_clipped_pct"]);
}

#[test]
fn brief_reports_keep_editing_metrics_without_changing_full_reports_or_pixels() {
    let f = Fixture::new("brief-metrics");
    let output = f.path("preview.png");
    for command in ["render", "edit", "auto"] {
        // Both auto calls must start from the same base recipe.
        let _ = std::fs::remove_file(f.recipe());
        let full = ok(&[command, s(&f.raw), "-o", s(&output), "--size", "160"]);
        let pixels = std::fs::read(&output).unwrap();
        let _ = std::fs::remove_file(f.recipe());
        let brief = ok(&[command, s(&f.raw), "-o", s(&output), "--size", "160", "--brief"]);
        assert_eq!(std::fs::read(&output).unwrap(), pixels);
        for key in [
            "output",
            "output_color_space",
            "recipe_file",
            "region",
            "sensor_clipped_pct",
            "sensor_clipped_scope",
            "white_balance",
        ] {
            assert_eq!(brief[key], full[key], "{command}: {key}");
        }
        assert_eq!(brief["output_bytes"], pixels.len());
        assert_eq!(brief["output_replaced"], true);
        assert_eq!(brief["recipe_saved"], command != "render");
        assert!(brief["warnings"].is_array());
        if command != "auto" {
            assert_eq!(brief["warnings"], serde_json::json!([]));
        }
        for key in [
            "width",
            "height",
            "clipped_highlights_pct",
            "channel_clipped_pct",
            "crushed_shadows_pct",
            "neutral_cast",
            "zones",
        ] {
            assert_eq!(brief["stats"][key], full["stats"][key], "{command}: {key}");
        }
        for key in ["p1", "p5", "p25", "p50", "p75", "p95", "p99"] {
            assert_eq!(brief["stats"]["tones"][key], full["stats"]["tones"][key]);
        }
        assert_eq!(brief["stats"]["tones"].as_object().unwrap().len(), 7);
        assert_eq!(brief["noise_reduction"]["strength"], full["noise_reduction"]["strength"]);
        assert_eq!(brief["noise_reduction"]["source"], full["noise_reduction"]["source"]);
        for key in ["histogram", "hues_pct", "chroma_mean", "chroma_p95", "channel_clipped_pct_rgb"] {
            assert!(full["stats"].get(key).is_some());
            assert!(brief["stats"].get(key).is_none());
        }
        assert!(full.get("recipe").is_some());
        assert_eq!(full["output_bytes"], brief["output_bytes"]);
        assert_eq!(full["quality"], brief["quality"]);
        assert!(full.get("recipe_saved").is_none());
        assert!(brief.get("recipe").is_none());
        assert!(brief.get("recipe_compatibility").is_none());
        if command == "auto" {
            let decisions = brief["decisions"].as_array().unwrap();
            let originals = full["decisions"].as_array().unwrap();
            assert_eq!(decisions.len(), originals.len());
            assert!(decisions.iter().zip(originals).any(|(a, b)| a != b));
            for (summary, original) in decisions.iter().zip(originals) {
                let summary = summary.as_str().unwrap();
                assert!(!summary.contains('\n') && summary.chars().count() <= 170, "{summary}");
                for qualifier in ["unresolved", "unreachable", "unavailable"] {
                    if original.as_str().unwrap().contains(qualifier) {
                        assert!(summary.contains(qualifier), "{summary}");
                    }
                }
            }
        }
    }
}

#[test]
fn brief_recipe_only_saves_and_render_trials_are_explicit() {
    let f = Fixture::new("brief-trial");
    for command in ["edit", "auto"] {
        let report = ok(&[command, s(&f.raw), "--set", "denoise=25", "--brief"]);
        assert_eq!(report["recipe_file"], s(&f.recipe()));
        assert_eq!(report["recipe_saved"], true);
        for key in ["output", "output_bytes", "output_replaced", "stats"] {
            assert_eq!(report[key], Value::Null);
        }
        assert_eq!(report["noise_reduction"]["strength"], 25.0);
    }
    let before = std::fs::read(f.recipe()).unwrap();
    let report = ok(&["render", s(&f.raw), "--set", "exposure=1", "--brief"]);
    assert_eq!(report["recipe_saved"], false);
    assert!(report["stats"]["tones"]["p50"].is_number());
    assert_eq!(std::fs::read(f.recipe()).unwrap(), before);
    std::fs::remove_file(f.recipe()).unwrap();
    assert_eq!(ok(&["render", s(&f.raw), "--brief"])["recipe_file"], Value::Null);
    assert!(!f.recipe().exists());
}

#[test]
fn brief_preserves_recipe_compatibility_and_unresolved_auto_warnings() {
    let f = Fixture::new("brief-warnings");
    std::fs::write(f.recipe(), br#"{"rawsmith":"0.1.0","exposure":0.5}"#).unwrap();
    let report = ok(&["render", s(&f.raw), "--brief"]);
    let warning = report["warnings"][0].as_str().unwrap();
    assert!(warning.contains("0.1.0") && warning.contains("current") && warning.contains("does not pin"));
    // The ramp's sensor-loss patch triggers an unresolved highlight target,
    // and its missing camera JPEG exercises the unavailable fallback notice.
    let full = ok(&["auto", s(&f.raw)]);
    std::fs::remove_file(f.recipe()).unwrap();
    let brief = ok(&["auto", s(&f.raw), "--brief"]);
    let originals = full["decisions"].as_array().unwrap();
    assert!(originals.iter().any(|note| note.as_str().unwrap().contains("unresolved")), "{full}");
    assert!(originals.iter().any(|note| note
        .as_str()
        .unwrap()
        .split(';')
        .skip(1)
        .any(|clause| clause.contains("unresolved"))));
    for (original, summary) in originals.iter().zip(brief["decisions"].as_array().unwrap()) {
        for qualifier in ["unresolved", "unreachable", "unavailable"] {
            if original.as_str().unwrap().contains(qualifier) {
                assert!(summary.as_str().unwrap().contains(qualifier));
                assert!(brief["warnings"].as_array().unwrap().contains(summary));
            }
        }
    }
}

#[test]
fn region_edit_reports_saved_whole_picture_settings_in_full_and_brief() {
    let f = Fixture::new("region-save");
    let output = f.path("detail.png");
    for brief in [false, true] {
        let mut args =
            vec!["edit", s(&f.raw), "--set", "exposure=0.75", "--region", "0.25,0.25,0.75,0.75", "-o", s(&output)];
        if brief {
            args.push("--brief");
        }
        let report = ok(&args);
        assert_eq!(report["recipe_saved"], true);
        assert_eq!(report["region"], serde_json::json!([0.25, 0.25, 0.75, 0.75]));
        assert_eq!(report["stats"]["width"], 160);
        assert_eq!(report["stats"]["height"], 120);
        let note = report["note"].as_str().unwrap();
        assert!(
            note.contains("whole picture")
                && note.contains("only the requested region")
                && note.contains("not a trial")
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(f.recipe()).unwrap()).unwrap();
        assert_eq!(saved["exposure"], 0.75);
        assert!(saved.get("region").is_none());
        let full = ok(&["render", s(&f.raw)]);
        assert_eq!(full["recipe"]["exposure"], 0.75);
        assert_eq!(full["stats"]["width"], 320);
        assert_eq!(full["stats"]["height"], 240);
    }
    let report = ok(&["edit", s(&f.raw), "--region", "0,0,0.5,0.5", "--brief"]);
    assert_eq!(report["recipe_saved"], true);
    assert_eq!(report["stats"], Value::Null);
    assert!(report["note"].as_str().unwrap().contains("no image was written"));
}

#[test]
fn gradients_append_remove_in_order_and_render_trials_do_not_save() {
    let f = Fixture::new("gradient-operations");
    let first = r#"gradients+={"from":[0.5,0],"to":[0.5,0.5],"exposure":-0.6}"#;
    let second = r#"gradients+={"from":[0.5,1],"to":[0.5,0.6],"exposure":0.5}"#;
    ok(&["edit", s(&f.raw), "--set", first]);
    let report = ok(&["edit", s(&f.raw), "--set", second]);
    assert_eq!(report["recipe"]["gradients"].as_array().unwrap().len(), 2);
    assert_eq!(report["recipe"]["gradients"][0]["exposure"], -0.6);
    assert_eq!(report["recipe"]["gradients"][1]["exposure"], 0.5);
    let saved = std::fs::read(f.recipe()).unwrap();
    let trial = ok(&["render", s(&f.raw), "--set", "gradients-=0", "--set", first]);
    assert_eq!(trial["recipe"]["gradients"][0]["exposure"], 0.5);
    assert_eq!(trial["recipe"]["gradients"][1]["exposure"], -0.6);
    assert_eq!(std::fs::read(f.recipe()).unwrap(), saved);
    let report = ok(&["edit", s(&f.raw), "--set", "gradients-=1", "--set", first, "--set", "gradients-=0"]);
    assert_eq!(report["recipe"]["gradients"].as_array().unwrap().len(), 1);
    assert_eq!(report["recipe"]["gradients"][0]["exposure"], -0.6);
    // Replacement and clearing keep their original meanings.
    let report = ok(&["edit", s(&f.raw), "--set", "gradients=[]", "--set", second]);
    assert_eq!(report["recipe"]["gradients"][0]["exposure"], 0.5);
    let report = ok(&["edit", s(&f.raw), "--set", "gradients-=0"]);
    assert!(report["recipe"].get("gradients").is_none());
}

#[test]
fn invalid_gradient_mutations_preserve_raw_recipe_and_output() {
    let f = Fixture::new("gradient-errors");
    let output = f.path("preview.png");
    let append = r#"gradients+={"from":[0.5,0],"to":[0.5,0.5],"exposure":-0.6}"#;
    ok(&[
        "edit",
        s(&f.raw),
        "--set",
        append,
        "--set",
        append,
        "--set",
        append,
        "--set",
        append,
        "-o",
        s(&output),
        "--size",
        "32",
    ]);
    let before: Vec<_> =
        [&f.raw, &f.recipe(), &output].into_iter().map(|p| (p.clone(), std::fs::read(p).unwrap())).collect();
    for (assignment, message) in [
        (append, "at most 4"),
        ("gradients-=4", "out of range"),
        ("gradients-=-1", "zero-based non-negative integer"),
        ("gradients-=1.0", "zero-based non-negative integer"),
        ("gradients-=1e0", "zero-based non-negative integer"),
        ("gradients-=+1", "zero-based non-negative integer"),
        ("gradients-=true", "zero-based non-negative integer"),
        ("gradients-=null", "zero-based non-negative integer"),
        ("gradients-=999999999999999999999999999999", "zero-based non-negative integer"),
        ("gradients+=[]", "one gradient JSON object"),
        ("gradients+=[[0,0],[0,0.5],0,0]", "one gradient JSON object"),
        ("gradients+=1", "one gradient JSON object"),
        ("gradients+={}", "one gradient JSON object"),
        ("gradients=null", "invalid gradients list"),
        ("gradients={}", "invalid gradients list"),
        ("gradients++={}", "gradients+"),
        ("gradients--=1", "gradients-"),
        (r#"gradients+={"from":[0,0],"to":[0,0]}"#, "at least 0.02"),
        (r#"gradients+={"from":[0,0],"to":[0,0.5],"exposure":4}"#, "exposure in -3..3"),
    ] {
        for command in ["edit", "render", "auto"] {
            let mut args = vec![command, s(&f.raw)];
            // Auto proposes fresh local settings before applying --set.
            if command == "auto" {
                for _ in 0..4 {
                    args.extend(["--set", append]);
                }
            }
            args.extend(["--set", assignment, "--set", "gradients-=0", "-o", s(&output), "--size", "32", "--brief"]);
            let (success, error) = run(&args);
            assert!(!success, "{command}: {assignment}");
            assert_eq!(error["error"]["kind"], "params");
            assert!(error["error"]["message"].as_str().unwrap().contains(message), "{error}");
            for (path, bytes) in &before {
                assert_eq!(std::fs::read(path).unwrap(), *bytes, "{command}: {assignment}: {path:?}");
            }
            assert_eq!(std::fs::read_dir(&f.dir).unwrap().count(), before.len());
        }
    }
    ok(&["edit", s(&f.raw), "--set", "gradients=[]"]);
    let (success, error) = run(&["edit", s(&f.raw), "--set", "gradients-=0"]);
    assert!(!success && error["error"]["message"].as_str().unwrap().contains("out of range for 0"));
}

#[test]
fn geometry_follows_orientation_crop_and_rotation() {
    let f = Fixture::new("geometry");
    let v = ok(&["render", s(&f.raw), "--set", "crop.left=0.5", "--set", "crop.bottom=0.5"]);
    assert_eq!((v["stats"]["width"].as_u64(), v["stats"]["height"].as_u64()), (Some(160), Some(120)));
    let v = ok(&["render", s(&f.raw), "--set", "rotate=5"]);
    let (w, h) = (v["stats"]["width"].as_u64().unwrap(), v["stats"]["height"].as_u64().unwrap());
    assert!(w < 320 && h < 240 && (w as f64 / h as f64 - 4.0 / 3.0).abs() < 0.02, "{w}x{h}");
}

#[test]
fn mistakes_produce_json_errors_and_change_nothing() {
    let f = Fixture::new("errors");
    for (args, kind) in [
        (vec!["render", s(&f.raw), "--set", "exposure=99"], "params"),
        (vec!["render", s(&f.raw), "--set", "exposrue=1"], "params"),
        (vec!["edit", s(&f.raw), "--set", "hsl.teal.hue=3"], "params"),
        (vec!["render", s(&f.raw), "-o", s(&f.path("x.bmp"))], "params"),
        (vec!["render", s(&f.raw), "-o", s(&f.raw)], "params"),
        (vec!["render", s(&f.raw), "--region", "0.5,0.5,0.2,0.9"], "params"),
        (vec!["render", s(&f.raw), "--region", "0,0,oops,1,1"], "params"),
        (vec!["render", s(&f.raw), "--region", "0,0,,1"], "params"),
        (vec!["render", s(&f.raw), "--size", "abc"], "usage"),
        (vec!["render"], "usage"),
        (vec!["frobnicate"], "usage"),
        (vec!["render", s(&f.raw), "--set", "temperature=2000", "--set", "tint=150"], "params"),
        (vec!["extract-jpeg", s(&f.raw), "-o", s(&f.raw)], "params"),
        (vec!["edit", s(&f.raw), "--reset", "--recipe", s(&f.raw)], "params"),
        (vec!["auto", s(&f.raw), "--recipe", s(&f.raw)], "params"),
        (vec!["inspect", s(&f.path("missing.ARW"))], "io"),
    ] {
        let (success, v) = run(&args);
        assert!(!success, "{args:?} should fail");
        assert_eq!(v["error"]["kind"], kind, "{args:?} -> {v}");
        assert!(v["error"]["message"].as_str().unwrap().len() > 10);
    }
    assert!(!f.recipe().exists(), "a rejected edit must not save a recipe");

    // The RAW is protected under every name it has: symlinks and hard links too.
    let before = std::fs::read(&f.raw).unwrap();
    let (hard, soft) = (f.path("hard.png"), f.path("soft.jpg"));
    std::fs::hard_link(&f.raw, &hard).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&f.raw, &soft).unwrap();
    for target in [&hard, &soft] {
        if target.exists() {
            for args in [
                vec!["render", s(&f.raw), "-o", s(target)],
                vec!["edit", s(&f.raw), "--reset", "--recipe", s(target)],
                vec!["extract-jpeg", s(&f.raw), "-o", s(target)],
            ] {
                let (success, v) = run(&args);
                assert!(!success && v["error"]["kind"] == "params", "{args:?} -> {v}");
            }
        }
    }
    assert_eq!(std::fs::read(&f.raw).unwrap(), before, "the RAW was modified");

    // A recipe and a picture cannot share a name, in either direction.
    std::fs::create_dir_all(f.path("sub")).unwrap();
    let shared = f.path("shared.jpg");
    for args in [
        vec!["edit", s(&f.raw), "--reset", "--recipe", s(&shared), "-o", s(&shared)],
        vec!["auto", s(&f.raw), "--recipe", s(&shared), "-o", s(&shared)],
        vec!["render", s(&f.raw), "--recipe", s(&shared), "-o", s(&shared)],
    ] {
        let (success, v) = run(&args);
        assert!(!success && v["error"]["kind"] == "params", "{args:?} -> {v}");
    }
    assert!(!shared.exists());
    let (success, v) =
        run(&["edit", s(&f.raw), "--reset", "--recipe", s(&f.dir.join("sub/../shared.jpg")), "-o", s(&shared)]);
    assert!(!success && v["error"]["kind"] == "params", "{v}");
    assert!(!shared.exists());

    // Saving a recipe leaves no staging file behind.
    ok(&["edit", s(&f.raw), "--set", "exposure=0.3"]);
    let leftovers: Vec<_> = std::fs::read_dir(&f.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    std::fs::remove_file(f.recipe()).unwrap();

    // The smallest accepted RAW still crops to at least one pixel.
    let tiny = f.path("tiny.ARW");
    std::fs::write(&tiny, Synthetic::new(16, 16, vec![3000; 256]).to_bytes()).unwrap();
    for settings in ["crop.top=0.95", "crop.left=0.95", "crop.bottom=0.05", "crop.right=0.05"] {
        let v = ok(&["render", s(&tiny), "--size", "8", "--set", settings, "-o", s(&f.path("tiny.png"))]);
        assert!(v["stats"]["width"].as_u64().unwrap() >= 1 && v["stats"]["height"].as_u64().unwrap() >= 1);
    }

    let junk = f.path("junk.ARW");
    std::fs::write(&junk, b"not a raw file at all").unwrap();
    assert_eq!(run(&["inspect", s(&junk)]).1["error"]["kind"], "format");

    let mut other = Synthetic::new(64, 48, vec![2000; 64 * 48]);
    other.model = "DSC-UNKNOWN".into();
    let unknown = f.path("unknown.ARW");
    std::fs::write(&unknown, other.to_bytes()).unwrap();
    let (_, v) = run(&["render", s(&unknown)]);
    assert_eq!(v["error"]["kind"], "unsupported");
    assert!(v["error"]["message"].as_str().unwrap().contains("ILCE-7M4"));
}

#[test]
fn schema_lists_every_parameter() {
    let v = ok(&["schema"]);
    let paths: Vec<&str> = v["parameters"].as_array().unwrap().iter().map(|p| p["path"].as_str().unwrap()).collect();
    for expected in [
        "exposure",
        "temperature",
        "hsl.aqua.luminance",
        "grade.balance",
        "crop.right",
        "defringe",
        "denoise",
        "sharpen",
    ] {
        assert!(paths.contains(&expected), "{expected} missing");
    }
    let unique: std::collections::HashSet<_> = paths.iter().collect();
    assert_eq!(paths.len(), unique.len(), "schema paths must be unique");
    assert!(v["parameters"].as_array().unwrap().iter().all(|p| p["description"].as_str().unwrap().len() > 15));
}

#[test]
fn auto_keeps_saved_defringe_and_allows_an_explicit_override() {
    let f = Fixture::new("auto-defringe");
    ok(&["edit", s(&f.raw), "--set", "defringe=65"]);
    let report = ok(&["auto", s(&f.raw)]);
    assert_eq!(report["recipe"]["defringe"], 65.0);
    let saved: Value = serde_json::from_slice(&std::fs::read(f.recipe()).unwrap()).unwrap();
    assert_eq!(saved["defringe"], 65.0);
    assert!(!report["decisions"].as_array().unwrap().iter().any(|d| d.as_str().unwrap().contains("overrides")));
    let report = ok(&["auto", s(&f.raw), "--set", "defringe=0"]);
    assert!(report["recipe"].get("defringe").is_none());
    // The analysis still describes the kept strength; the report says what changed afterwards.
    let last = report["decisions"].as_array().unwrap().last().unwrap().as_str().unwrap();
    assert!(last.starts_with("overrides applied after this analysis: defringe=0;"), "{last}");
    let brief = ok(&["auto", s(&f.raw), "--set", "defringe=0", "--set", "exposure=0.5", "--brief"]);
    assert_eq!(
        brief["decisions"].as_array().unwrap().last().unwrap(),
        "overrides applied after this analysis: defringe=0, exposure=0.5"
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(f.recipe()).unwrap()).unwrap();
    assert!(saved.get("defringe").is_none());
}

#[test]
fn recipe_only_edits_validate_the_source_and_preserve_existing_edits_on_failure() {
    let f = Fixture::new("recipe-validation");
    ok(&["edit", s(&f.raw), "--set", "exposure=0.5"]);
    let before = std::fs::read(f.recipe()).unwrap();
    let junk = f.path("junk.ARW");
    std::fs::write(&junk, b"not a raw file").unwrap();
    let unknown = f.path("unknown.ARW");
    let mut raw = Synthetic::new(64, 48, vec![2000; 64 * 48]);
    raw.model = "DSC-UNKNOWN".into();
    std::fs::write(&unknown, raw.to_bytes()).unwrap();
    for (source, kind) in [(&f.path("missing.ARW"), "io"), (&junk, "format"), (&unknown, "unsupported")] {
        let (success, v) = run(&["edit", s(source), "--recipe", s(&f.recipe()), "--set", "exposure=1"]);
        assert!(!success && v["error"]["kind"] == kind, "{v}");
        assert_eq!(std::fs::read(f.recipe()).unwrap(), before);
    }
    for command in ["edit", "auto"] {
        let (success, v) = run(&[command, s(&f.raw), "--set", "temperature=2000", "--set", "tint=150"]);
        assert!(!success && v["error"]["kind"] == "params", "{command}: {v}");
        assert_eq!(std::fs::read(f.recipe()).unwrap(), before);
    }
    let (success, v) = run(&["edit", s(&f.path("missing.ARW")), "--set", "exposure=1"]);
    assert!(!success && v["error"]["kind"] == "io", "{v}");
    assert!(!f.path("missing.ARW.rawsmith.json").exists());
}

#[test]
fn invalid_preview_options_fail_before_changing_any_files() {
    let f = Fixture::new("target-validation");
    let preview = f.path("preview.png");
    std::fs::write(&preview, b"previous preview").unwrap();
    ok(&["edit", s(&f.raw), "--set", "exposure=0.5"]);
    let recipe = std::fs::read(f.recipe()).unwrap();
    for command in ["render", "edit", "auto"] {
        for (flag, value) in [
            ("--size", "0"),
            ("--quality", "0"),
            ("--quality", "200"),
            ("--region", "bad"),
            ("--region", "0.999999999,0,1,1"),
            ("--region", "0,0.999999999,1,1"),
        ] {
            for output in [false, true] {
                let mut args = vec![command, s(&f.raw), flag, value];
                if output {
                    args.extend(["-o", s(&preview)]);
                }
                let (success, v) = run(&args);
                assert!(!success && v["error"]["kind"] == "params", "{args:?}: {v}");
                assert_eq!(std::fs::read(f.recipe()).unwrap(), recipe);
                assert_eq!(std::fs::read(&preview).unwrap(), b"previous preview");
            }
        }
    }
    for quality in ["1", "100"] {
        let report = ok(&["render", s(&f.raw), "--size", "1", "--quality", quality, "-o", s(&f.path("tiny.jpg"))]);
        assert_eq!(report["stats"]["width"], 1);
    }
}

#[test]
fn render_requires_an_explicit_recipe_to_exist_but_edit_can_create_it() {
    let f = Fixture::new("explicit-recipe");
    let custom = f.path("custom.json");
    let preview = f.path("preview.png");
    std::fs::write(&preview, b"previous preview").unwrap();
    let (success, v) = run(&["render", s(&f.raw), "--recipe", s(&custom), "-o", s(&preview)]);
    assert!(!success && v["error"]["kind"] == "io", "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("custom.json"));
    assert_eq!(std::fs::read(&preview).unwrap(), b"previous preview");
    assert!(!custom.exists());
    assert_eq!(ok(&["render", s(&f.raw)])["recipe"], serde_json::json!({}));
    ok(&["edit", s(&f.raw), "--recipe", s(&custom), "--set", "exposure=0.75"]);
    assert_eq!(ok(&["render", s(&f.raw), "--recipe", s(&custom)])["recipe"]["exposure"], 0.75);
    std::fs::remove_file(&custom).unwrap();
    ok(&["auto", s(&f.raw), "--recipe", s(&custom)]);
    assert!(custom.exists());
}

#[test]
fn overflowing_white_balance_increment_does_not_clear_the_saved_value() {
    let f = Fixture::new("overflow");
    ok(&["edit", s(&f.raw), "--set", "temperature=5200", "--set", "tint=12"]);
    let before = std::fs::read(f.recipe()).unwrap();
    for set in ["temperature+=1e308", "temperature-=1e308", "tint+=1e308", "tint-=1e308"] {
        let (success, v) = run(&["edit", s(&f.raw), "--set", set]);
        assert!(!success && v["error"]["kind"] == "params", "{set}: {v}");
        assert_eq!(std::fs::read(f.recipe()).unwrap(), before);
    }
}

#[test]
fn failed_recipe_save_preserves_the_preview_and_leaves_no_staging_files() {
    let f = Fixture::new("save-failure");
    let preview = f.path("preview.png");
    let directory = f.path("recipe-directory");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("keep.txt"), b"keep").unwrap();
    for command in ["edit", "auto"] {
        for recipe in [&f.path("missing/recipe.json"), &directory] {
            for existing_preview in [false, true] {
                if existing_preview {
                    std::fs::write(&preview, b"previous preview").unwrap();
                }
                let (success, v) = run(&[command, s(&f.raw), "--recipe", s(recipe), "-o", s(&preview)]);
                assert!(!success && v["error"]["kind"] == "io", "{command}: {v}");
                if existing_preview {
                    assert_eq!(std::fs::read(&preview).unwrap(), b"previous preview");
                    std::fs::remove_file(&preview).unwrap();
                } else {
                    assert!(!preview.exists());
                }
                assert_eq!(std::fs::read(directory.join("keep.txt")).unwrap(), b"keep");
                assert!(!f.path("missing").exists());
                assert!(std::fs::read_dir(&f.dir).unwrap().all(|entry| {
                    let name = entry.unwrap().file_name();
                    !name.to_string_lossy().ends_with(".tmp") && !name.to_string_lossy().ends_with(".bak")
                }));
            }
        }
    }
    // A reset bypasses recipe loading, so a directory reaches the commit stage.
    std::fs::write(&preview, b"previous preview").unwrap();
    let (success, v) = run(&["edit", s(&f.raw), "--reset", "--recipe", s(&directory), "-o", s(&preview)]);
    assert!(!success && v["error"]["kind"] == "io", "{v}");
    assert_eq!(std::fs::read(&preview).unwrap(), b"previous preview");
    assert_eq!(std::fs::read(directory.join("keep.txt")).unwrap(), b"keep");
}

#[test]
fn extracted_camera_jpeg_keeps_metadata_while_developed_images_do_not() {
    let f = Fixture::new("metadata");
    let reference = f.path("reference.jpg");
    ok(&["render", s(&f.raw), "-o", s(&reference)]);
    let jpeg = std::fs::read(&reference).unwrap();
    let metadata = b"Exif\0\0synthetic-metadata-only";
    let mut embedded = jpeg[..2].to_vec();
    embedded.extend([0xff, 0xe1]);
    embedded.extend(((metadata.len() + 2) as u16).to_be_bytes());
    embedded.extend(metadata);
    embedded.extend(&jpeg[2..]);

    // Add a second top-level TIFF IFD pointing at the embedded JPEG.
    let mut raw = std::fs::read(&f.raw).unwrap();
    let ifd = raw.len() as u32;
    raw[58..62].copy_from_slice(&ifd.to_le_bytes());
    let offset = ifd + 2 + 2 * 12 + 4;
    raw.extend(2u16.to_le_bytes());
    for (tag, value) in [(0x0201u16, offset), (0x0202, embedded.len() as u32)] {
        raw.extend(tag.to_le_bytes());
        raw.extend(4u16.to_le_bytes());
        raw.extend(1u32.to_le_bytes());
        raw.extend(value.to_le_bytes());
    }
    raw.extend(0u32.to_le_bytes());
    raw.extend(&embedded);
    std::fs::write(&f.raw, &raw).unwrap();

    let extracted = f.path("camera.jpg");
    ok(&["extract-jpeg", s(&f.raw), "-o", s(&extracted)]);
    assert_eq!(std::fs::read(extracted).unwrap(), embedded);
    for command in ["render", "edit", "auto"] {
        let developed = f.path(&format!("{command}.jpg"));
        ok(&[command, s(&f.raw), "-o", s(&developed)]);
        assert!(!std::fs::read(developed).unwrap().windows(metadata.len()).any(|bytes| bytes == metadata));
    }
    assert_eq!(std::fs::read(&f.raw).unwrap(), raw);
}

/// Append an IFD pointing at a synthetic JPEG without touching the RAW mosaic.
fn embed_jpeg(raw: &mut Vec<u8>, jpeg: &[u8]) {
    let ifd = raw.len() as u32;
    raw[58..62].copy_from_slice(&ifd.to_le_bytes());
    let offset = ifd + 2 + 2 * 12 + 4;
    raw.extend(2u16.to_le_bytes());
    for (tag, value) in [(0x0201u16, offset), (0x0202, jpeg.len() as u32)] {
        raw.extend(tag.to_le_bytes());
        raw.extend(4u16.to_le_bytes());
        raw.extend(1u32.to_le_bytes());
        raw.extend(value.to_le_bytes());
    }
    raw.extend(0u32.to_le_bytes());
    raw.extend(jpeg);
}

#[test]
fn extract_reports_orientation_without_rotating_or_changing_bytes() {
    let f = Fixture::new("extract-orientation");
    let image = rawsmith::image::Image::new(64, 48);
    let jpeg = rawsmith::output::encode(Path::new("camera.jpg"), &image, 92).unwrap();
    let output = f.path("camera.jpg");
    for (orientation, rotation) in [(1, Some(0)), (3, Some(180)), (6, Some(90)), (8, Some(270)), (2, None), (9, None)] {
        let mut file = Synthetic::new(64, 48, vec![2000; 64 * 48]);
        file.orientation = orientation;
        let mut raw = file.to_bytes();
        embed_jpeg(&mut raw, &jpeg);
        std::fs::write(&f.raw, &raw).unwrap();
        let report = ok(&["extract-jpeg", s(&f.raw), "-o", s(&output)]);
        assert_eq!(report["raw_orientation"], orientation);
        assert_eq!(report["rotation_degrees_clockwise"], serde_json::json!(rotation));
        assert_eq!(report["needs_rotation"], serde_json::json!(rotation.map(|degrees| degrees != 0)));
        assert_eq!(report["output_replaced"], orientation != 1);
        assert_eq!(std::fs::read(&output).unwrap(), jpeg);
        assert_eq!(std::fs::read(&f.raw).unwrap(), raw);
        if matches!(orientation, 6 | 8) {
            let rendered = ok(&["render", s(&f.raw)]);
            assert_eq!(
                (rendered["stats"]["width"].as_u64(), rendered["stats"]["height"].as_u64()),
                (Some(48), Some(64))
            );
        }
    }
}

#[test]
fn inspect_distinguishes_lossy_compression_from_uncompressed() {
    let f = Fixture::new("compression");
    let mut raw = Synthetic::new(64, 48, vec![2000; 64 * 48]).to_bytes();
    // Change the sensor IFD's Compression tag; all-zero 16-byte blocks are
    // valid Sony lossy blocks. No actual photo or new decoder is needed.
    let raw_ifd = u32::from_le_bytes(raw[54..58].try_into().unwrap()) as usize;
    let compression_value = raw_ifd + 2 + 3 * 12 + 8;
    raw[compression_value..compression_value + 4].copy_from_slice(&32767u32.to_le_bytes());
    let strip_entry = raw_ifd + 2 + 5 * 12 + 8;
    let strip = u32::from_le_bytes(raw[strip_entry..strip_entry + 4].try_into().unwrap()) as usize;
    raw[strip..].fill(0);
    std::fs::write(&f.raw, &raw).unwrap();
    assert_eq!(ok(&["inspect", s(&f.raw)])["compression"], "lossy-compressed");
    // An unknown encoding must remain an error, not be labelled as lossy.
    raw[compression_value..compression_value + 4].copy_from_slice(&65535u32.to_le_bytes());
    std::fs::write(&f.raw, &raw).unwrap();
    let (success, error) = run(&["inspect", s(&f.raw)]);
    assert!(!success && error["error"]["kind"] == "unsupported", "{error}");
}

#[test]
fn render_reports_the_loaded_recipe_and_trials_never_save_it() {
    let f = Fixture::new("recipe-report");
    let trial = ok(&["render", s(&f.raw), "--set", "exposure=0.25"]);
    assert_eq!(trial.get("recipe_file"), Some(&Value::Null));
    assert_eq!(trial["recipe"]["exposure"], 0.25);
    assert!(!f.recipe().exists());
    assert_eq!(ok(&["edit", s(&f.raw)])["recipe_file"], s(&f.recipe()));
    let before = std::fs::read(f.recipe()).unwrap();
    assert_eq!(ok(&["render", s(&f.raw), "--set", "exposure=0.5"])["recipe_file"], s(&f.recipe()));
    assert_eq!(std::fs::read(f.recipe()).unwrap(), before);
    let explicit = f.path("custom.json");
    assert_eq!(ok(&["auto", s(&f.raw), "--recipe", s(&explicit)])["recipe_file"], s(&explicit));
    assert_eq!(ok(&["render", s(&f.raw), "--recipe", s(&explicit)])["recipe_file"], s(&explicit));
}

#[test]
fn developed_reports_declare_srgb_and_signal_output_replacement() {
    let f = Fixture::new("replacement-report");
    for command in ["render", "edit", "auto"] {
        for format in ["jpg", "png"] {
            let output = f.path(&format!("{command}.{format}"));
            for replaced in [false, true] {
                let report = ok(&[command, s(&f.raw), "-o", s(&output), "--size", "16"]);
                assert_eq!(report["output_color_space"], "sRGB");
                assert_eq!(report["output_replaced"], replaced);
            }
        }
        let report = ok(&[command, s(&f.raw)]);
        assert_eq!(report.get("output_color_space"), Some(&Value::Null));
        assert_eq!(report.get("output_replaced"), Some(&Value::Null));
    }
    #[cfg(unix)]
    {
        for command in ["render", "edit", "auto"] {
            let output = f.path(&format!("{command}-dangling.jpg"));
            std::os::unix::fs::symlink("missing.jpg", &output).unwrap();
            assert_eq!(ok(&[command, s(&f.raw), "-o", s(&output), "--size", "16"])["output_replaced"], true);
            assert!(!f.path("missing.jpg").exists());
        }
    }
}

#[test]
fn io_errors_name_paths_and_preserve_recipes_outputs_and_raws() {
    let f = Fixture::new("io-paths");
    let preview = f.path("preview.jpg");
    ok(&["edit", s(&f.raw), "--set", "exposure=0.5", "-o", s(&preview), "--size", "16"]);
    let raw = std::fs::read(&f.raw).unwrap();
    let recipe = std::fs::read(f.recipe()).unwrap();
    let image = std::fs::read(&preview).unwrap();
    let missing_output = f.path("missing/preview.jpg");
    let missing_recipe = f.path("missing/recipe.json");
    let missing_raw = f.path("missing.ARW");
    let directory = f.path("directory.jpg");
    std::fs::create_dir(&directory).unwrap();
    // Recipe reads must name their path too, including non-file destinations.
    for path in [&missing_recipe, &directory] {
        let (success, error) = run(&["render", s(&f.raw), "--recipe", s(path), "-o", s(&preview)]);
        assert!(!success && error["error"]["kind"] == "io", "{error}");
        assert!(error["error"]["message"].as_str().unwrap().contains(s(path)), "{error}");
    }
    for command in ["render", "edit", "auto"] {
        for path in [&missing_output, &directory] {
            let (success, error) = run(&[command, s(&f.raw), "-o", s(path), "--size", "16"]);
            assert!(!success && error["error"]["kind"] == "io", "{error}");
            assert!(error["error"]["message"].as_str().unwrap().contains(s(path)), "{error}");
        }
    }
    for command in ["edit", "auto"] {
        for output in [false, true] {
            let mut args = vec![command, s(&f.raw), "--recipe", s(&missing_recipe)];
            if output {
                args.extend(["-o", s(&preview), "--size", "16"]);
            }
            let (_, error) = run(&args);
            assert!(error["error"]["message"].as_str().unwrap().contains(s(&missing_recipe)), "{error}");
        }
    }
    for command in ["inspect", "render", "edit", "auto", "extract-jpeg"] {
        let mut args = vec![command, s(&missing_raw)];
        if command == "extract-jpeg" {
            args.extend(["-o", s(&preview)]);
        }
        let (_, error) = run(&args);
        assert!(error["error"]["message"].as_str().unwrap().contains(s(&missing_raw)), "{error}");
    }
    // Extraction has its own write path, and must preserve an old output on failure.
    let mut with_jpeg = raw.clone();
    embed_jpeg(&mut with_jpeg, &image);
    let camera_raw = f.path("camera.ARW");
    std::fs::write(&camera_raw, &with_jpeg).unwrap();
    for path in [&missing_output, &directory] {
        let (_, error) = run(&["extract-jpeg", s(&camera_raw), "-o", s(path)]);
        assert!(error["error"]["message"].as_str().unwrap().contains(s(path)), "{error}");
    }
    assert_eq!(std::fs::read(&f.raw).unwrap(), raw);
    assert_eq!(std::fs::read(f.recipe()).unwrap(), recipe);
    assert_eq!(std::fs::read(&preview).unwrap(), image);
    assert!(!missing_recipe.exists() && !missing_output.exists());
    assert!(std::fs::read_dir(&f.dir).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[test]
fn help_explains_overwrites_colour_orientation_and_error_contract() {
    for command in [None, Some("render"), Some("edit"), Some("auto"), Some("extract-jpeg")] {
        let mut args = Vec::new();
        if let Some(command) = command {
            args.push(command);
        }
        args.push("--help");
        let output = Command::new(env!("CARGO_BIN_EXE_rawsmith")).args(&args).output().unwrap();
        assert!(output.status.success() && output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        if matches!(command, Some("render" | "edit" | "auto")) {
            for text in [
                "--brief",
                "gradients+={...}",
                "gradients-=INDEX",
                "zero-based",
                "whole picture",
                "render --set is a trial",
            ] {
                assert!(help.contains(text), "{help}");
            }
        }
        if command.is_some() {
            assert!(!help.contains("keep a version") && !help.contains("RAWs"), "{help}");
            assert_eq!(help.matches("output_replaced").count(), 1, "{help}");
            assert!(help.contains("reports whether a file was replaced"), "{help}");
        } else {
            assert_eq!(help.matches("outputs are replaced").count(), 1, "{help}");
            assert_eq!(help.matches("keep a version").count(), 1, "{help}");
            assert_eq!(help.matches("RAWs are never written").count(), 1, "{help}");
        }
        if command == Some("extract-jpeg") {
            assert!(
                help.contains("raw_orientation")
                    && help.contains("needs_rotation")
                    && help.contains("not rotated")
                    && help.contains("output_replaced")
            );
        } else if command.is_some() {
            assert!(help.contains("sRGB") && help.contains("output_color_space") && help.contains("output_replaced"));
        } else {
            assert!(help.contains("exit status 1") && help.contains("empty stdout"));
        }
    }
}

#[test]
fn failures_before_commit_preserve_existing_recipes_and_outputs() {
    let f = Fixture::new("failure-contract");
    let preview = f.path("preview.jpg");
    let recipe_path = f.recipe();
    ok(&["edit", s(&f.raw), "--set", "exposure=0.5", "-o", s(&preview), "--size", "16"]);
    let raw = std::fs::read(&f.raw).unwrap();
    let recipe = std::fs::read(f.recipe()).unwrap();
    let image = std::fs::read(&preview).unwrap();
    let invalid_raw = f.path("invalid.ARW");
    let mut damaged = raw.clone();
    damaged[..4].fill(0);
    std::fs::write(&invalid_raw, &damaged).unwrap();
    let unsupported_output = f.path("preview.bmp");
    std::fs::write(&unsupported_output, b"previous unsupported output").unwrap();

    for command in ["render", "edit", "auto"] {
        for (source, output, settings, kind) in [
            (&f.raw, &preview, vec!["--set", "exposure=99"], "params"),
            (&invalid_raw, &preview, vec![], "format"),
            (&f.raw, &preview, vec!["--set", "temperature=2000", "--set", "tint=150"], "params"),
            (&f.raw, &unsupported_output, vec![], "params"),
        ] {
            let mut args = vec![command, s(source), "--recipe", s(&recipe_path), "-o", s(output), "--size", "16"];
            args.extend(settings);
            let (success, error) = run(&args);
            assert!(!success && error["error"]["kind"] == kind, "{args:?}: {error}");
            assert_eq!(std::fs::read(f.recipe()).unwrap(), recipe);
            assert_eq!(std::fs::read(&preview).unwrap(), image);
            assert_eq!(std::fs::read(&unsupported_output).unwrap(), b"previous unsupported output");
        }
    }
    let (success, error) = run(&["extract-jpeg", s(&f.raw), "-o", s(&preview)]);
    assert!(!success && error["error"]["kind"] == "unsupported", "{error}");
    assert_eq!(std::fs::read(&preview).unwrap(), image);
    assert_eq!(std::fs::read(&f.raw).unwrap(), raw);
    assert_eq!(std::fs::read(&invalid_raw).unwrap(), damaged);
    assert!(std::fs::read_dir(&f.dir).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[test]
fn small_preview_clipping_statistics_can_differ_from_the_full_export() {
    let f = Fixture::new("preview-clipping");
    let full = ok(&["render", s(&f.raw), "--set", "exposure=2", "-o", s(&f.path("full.png"))]);
    let preview = ok(&["render", s(&f.raw), "--set", "exposure=2", "--size", "1", "-o", s(&f.path("preview.png"))]);
    assert!(full["stats"]["clipped_highlights_pct"].as_f64().unwrap() > 0.0, "{full}");
    assert_eq!(preview["stats"]["clipped_highlights_pct"], 0.0, "{preview}");
}

#[test]
fn denoise_schema_reports_and_legacy_recipe_migration_are_explicit() {
    let f = Fixture::new("default-look-migration");
    let schema = ok(&["schema"]);
    let field = schema["parameters"].as_array().unwrap().iter().find(|p| p["path"] == "denoise").unwrap();
    assert!(field["default"].is_null());
    assert!(field["description"].as_str().unwrap().contains("native sensor noise"));
    let default = ok(&["render", s(&f.raw), "--size", "64"]);
    assert!(default["recipe_compatibility"].is_null());
    assert!(default["noise_reduction"]["strength"].is_number());
    assert!(default["recipe"].get("denoise").is_none());
    for version in [Some("0.1.0"), Some("0.1.1"), None, Some("0.2.0"), Some("99.0.0"), Some(env!("CARGO_PKG_VERSION"))]
    {
        let mut recipe = serde_json::json!({"exposure":0.1});
        if let Some(version) = version {
            recipe["rawsmith"] = serde_json::json!(version);
        }
        std::fs::write(f.recipe(), recipe.to_string()).unwrap();
        let before = std::fs::read(f.recipe()).unwrap();
        let rendered = ok(&["render", s(&f.raw), "--size", "64"]);
        let compatibility = &rendered["recipe_compatibility"];
        assert_eq!(compatibility["written_version"], serde_json::json!(version));
        assert_eq!(compatibility["rendering_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(compatibility["policy"], "current_defaults");
        // Defaults last changed in 0.2.0: only older, unknown or future writers get a note.
        assert_eq!(
            compatibility["note"].is_null(),
            matches!(version, Some("0.2.0")) || version == Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(rendered["recipe"]["exposure"], 0.1);
        let compared = ok(&[
            "compare",
            s(&f.raw),
            "--panel",
            "saved",
            "--panel",
            "default",
            "-o",
            s(&f.path("row.png")),
            "--size",
            "64",
        ]);
        assert_eq!(compared["panels"][0]["recipe_compatibility"], *compatibility);
        assert!(compared["panels"][1]["recipe_compatibility"].is_null());
        assert_eq!(compared["panels"][0]["noise_reduction"], rendered["noise_reduction"]);
        assert_eq!(std::fs::read(f.recipe()).unwrap(), before, "trial and compare must not migrate on disk");
    }
    for value in ["0", "10", "52.44140625", "100"] {
        let report = ok(&["edit", s(&f.raw), "--set", &format!("denoise={value}")]);
        assert_eq!(report["noise_reduction"]["strength"], value.parse::<f64>().unwrap());
        assert_eq!(report["noise_reduction"]["source"], "recipe");
        assert_eq!(ok(&["render", s(&f.raw), "--size", "64"])["noise_reduction"], report["noise_reduction"]);
        assert_eq!(ok(&["auto", s(&f.raw)])["noise_reduction"], report["noise_reduction"]);
    }
    let before = std::fs::read(f.recipe()).unwrap();
    let trial = ok(&["render", s(&f.raw), "--set", "denoise=null", "--size", "64"]);
    assert_ne!(trial["noise_reduction"]["source"], "recipe");
    assert_eq!(std::fs::read(f.recipe()).unwrap(), before);
    ok(&["edit", s(&f.raw), "--set", "denoise=null"]);
    let before = std::fs::read(f.recipe()).unwrap();
    let (success, _) = run(&["edit", s(&f.raw), "--set", "denoise+=5", "-o", s(&f.path("rejected.png"))]);
    assert!(!success);
    assert_eq!(std::fs::read(f.recipe()).unwrap(), before);
    assert!(!f.path("rejected.png").exists());
}

#[test]
fn masks_mutations_schema_diagnostic_and_errors_preserve_files() {
    let f = Fixture::new("masks");
    let radial = r#"masks+={"shape":"radial","center":[0.5,0.5],"radii":[0.3,0.2],"exposure":0.5}"#;
    let luma = r#"masks+={"luma":[0.7,1],"exposure":-0.5}"#;
    let report = ok(&["edit", s(&f.raw), "--set", radial, "--set", luma]);
    assert_eq!(report["recipe"]["masks"].as_array().unwrap().len(), 2);
    assert!(report["recipe"]["masks"][1].get("shape").is_none());
    let saved = std::fs::read(f.recipe()).unwrap();
    let raw = std::fs::read(&f.raw).unwrap();
    let output = f.path("mask.png");
    let report = ok(&["render", s(&f.raw), "--show-mask", "0", "-o", s(&output), "--size", "32", "--brief"]);
    assert_eq!(report["show_mask"]["index"], 0);
    assert_eq!(report["recipe_saved"], false);
    let existing = std::fs::read(&output).unwrap();
    let report = ok(&["render", s(&f.raw), "--set", "masks-=0", "--set", radial]);
    assert_eq!(report["recipe"]["masks"][0]["luma"][0], 0.7);
    for assignment in [
        "masks-=2",
        "masks-=-1",
        "masks-=1.0",
        "masks+=[]",
        r#"masks+=["linear",[0,0],[1,1],null,null,0,0.5,false,null,0.05,0.5,0,0,0,0]"#,
        r#"masks=[["linear",[0,0],[1,1],null,null,0,0.5,false,null,0.05,0.5,0,0,0,0]]"#,
        "masks++={}",
        "masks={}",
        "masks=null",
        r#"masks+={"shape":"radial","center":[0.5,0.5],"radii":[0,0.2]}"#,
        r#"masks+={"luma":[0.8,0.2]}"#,
        r#"masks+={"luma":[0.2,1],"temperatur":5}"#,
        r#"masks+={"luma":[0.2,1],"exposure":4}"#,
    ] {
        let (success, error) = run(&["edit", s(&f.raw), "--set", assignment, "-o", s(&output)]);
        assert!(!success, "{assignment}");
        assert_eq!(error["error"]["kind"], "params");
        assert_eq!(std::fs::read(f.recipe()).unwrap(), saved);
        assert_eq!(std::fs::read(&output).unwrap(), existing);
        assert_eq!(std::fs::read(&f.raw).unwrap(), raw);
    }
    let (success, error) = run(&["render", s(&f.raw), "--show-mask", "2", "-o", s(&output)]);
    assert!(!success);
    assert!(error["error"]["message"].as_str().unwrap().contains("out of range"));
    assert_eq!(std::fs::read(&output).unwrap(), existing);
    let schema = ok(&["schema"]);
    assert_eq!(schema["local_masks"]["max_entries"], 8);
    for name in [
        "shape",
        "radii",
        "feather",
        "luma",
        "luma_feather",
        "exposure",
        "saturation",
        "contrast",
        "shadows",
        "highlights",
    ] {
        assert!(schema["local_masks"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == name && v["description"].is_string()));
    }
    ok(&["edit", s(&f.raw), "--set", "masks=[]"]);
    for _ in 0..8 {
        ok(&["edit", s(&f.raw), "--set", luma]);
    }
    let (success, error) = run(&["edit", s(&f.raw), "--set", luma, "--set", "masks-=0"]);
    assert!(!success);
    assert!(error["error"]["message"].as_str().unwrap().contains("at most 8"));
}

#[test]
fn export_quality_is_reported_in_full_and_brief_without_entering_recipes() {
    let f = Fixture::new("export-quality-reports");
    for command in ["render", "edit", "auto"] {
        for brief in [false, true] {
            for extension in ["jpg", "png"] {
                let path = f.path(&format!("out.{extension}"));
                let mut args = vec![command, s(&f.raw), "-o", s(&path), "--quality", "85"];
                if brief {
                    args.push("--brief");
                }
                let report = ok(&args);
                assert_eq!(report["output_bytes"], std::fs::metadata(&path).unwrap().len());
                assert!(report.get("quality").is_some());
                assert_eq!(report["quality"], if extension == "jpg" { serde_json::json!(85) } else { Value::Null });
                assert!(report["quality_limited_by"].is_null());
            }
        }
        if command != "render" {
            for brief in [false, true] {
                let mut args = vec![command, s(&f.raw)];
                if brief {
                    args.push("--brief");
                }
                let report = ok(&args);
                assert_eq!(report.get("quality"), Some(&Value::Null));
                assert_eq!(report.get("output_bytes"), Some(&Value::Null));
            }
            let recipe: Value = serde_json::from_slice(&std::fs::read(f.recipe()).unwrap()).unwrap();
            assert!(recipe.get("quality").is_none());
            assert!(recipe.get("max_bytes").is_none());
        }
    }
}

#[test]
fn max_bytes_selects_highest_fitting_quality_deterministically() {
    let f = Fixture::new("export-byte-limit");
    let raw = rawsmith::arw::decode(&std::fs::read(&f.raw).unwrap()).unwrap();
    let rendered = rawsmith::pipeline::render(
        &raw,
        &rawsmith::params::Params::default(),
        rawsmith::pipeline::Size { max_edge: None },
    )
    .unwrap();
    let path = f.path("out.jpg");
    let encodings: Vec<_> = (60..=92).map(|q| rawsmith::output::encode(&path, &rendered.image, q).unwrap()).collect();
    for index in [0, 12, 25, 32] {
        let limit = encodings[index].len();
        let expected = encodings.iter().rposition(|bytes| bytes.len() <= limit).unwrap();
        for brief in [false, true] {
            let limit_text = limit.to_string();
            let mut args = vec!["render", s(&f.raw), "-o", s(&path), "--max-bytes", &limit_text];
            if brief {
                args.push("--brief");
            }
            let first = ok(&args);
            assert_eq!(first["quality"], 60 + expected);
            assert_eq!(first["output_bytes"], encodings[expected].len());
            assert_eq!(std::fs::read(&path).unwrap(), encodings[expected]);
            assert_eq!(
                first["quality_limited_by"],
                if expected < 32 { serde_json::json!("max_bytes") } else { Value::Null }
            );
            let second = ok(&args);
            assert_eq!(first["quality"], second["quality"]);
            assert_eq!(std::fs::read(&path).unwrap(), encodings[expected]);
        }
    }
    for command in ["edit", "auto"] {
        let report =
            ok(&[command, s(&f.raw), "-o", s(&path), "--max-bytes", "1MB", "--quality", "85", "--size", "160"]);
        assert_eq!(report["quality"], 85);
        assert!(report["quality_limited_by"].is_null());
    }
}

#[test]
fn byte_limit_errors_preserve_files_and_validate_units() {
    let f = Fixture::new("export-byte-errors");
    let path = f.path("out.jpg");
    let recipe = b"{\"exposure\":0.25}";
    std::fs::write(f.recipe(), recipe).unwrap();
    for command in ["render", "edit", "auto"] {
        for existing in [false, true] {
            if existing {
                std::fs::write(&path, b"old image").unwrap();
            }
            let (success, report) = run(&[command, s(&f.raw), "-o", s(&path), "--max-bytes", "1"]);
            assert!(!success);
            assert_eq!(report["error"]["kind"], "params");
            let message = report["error"]["message"].as_str().unwrap();
            assert!(
                message.contains("smallest size reached")
                    && message.contains("quality 60")
                    && message.contains("--size")
            );
            assert_eq!(std::fs::read(f.recipe()).unwrap(), recipe);
            if existing {
                assert_eq!(std::fs::read(&path).unwrap(), b"old image");
                std::fs::remove_file(&path).unwrap();
            } else {
                assert!(!path.exists());
            }
        }
    }
    for value in ["1GB", "0", "-1", "18446744073709551615MB", "1.2.3MB", "1.0001K"] {
        assert!(!run(&["render", s(&f.raw), "-o", s(&path), "--max-bytes", value]).0);
        assert!(!path.exists());
    }
    let png = f.path("out.png");
    std::fs::write(&png, b"old png").unwrap();
    assert!(!run(&["render", s(&f.raw), "-o", s(&png), "--max-bytes", "1MB"]).0);
    assert_eq!(std::fs::read(&png).unwrap(), b"old png");
    assert!(!run(&["edit", s(&f.raw), "--max-bytes", "1MB"]).0);
    assert!(!run(&["render", s(&f.raw), "-o", s(&path), "--quality", "59", "--max-bytes", "1MB"]).0);
    for value in ["1000000", "1000K", "1000KB", "1M", "1MB", "1.0mb"] {
        let report = ok(&["render", s(&f.raw), "-o", s(&path), "--max-bytes", value]);
        assert_eq!(report["quality"], 92);
    }
}

#[test]
fn export_options_are_discoverable_in_help() {
    for command in [None, Some("render"), Some("edit"), Some("auto"), Some("compare")] {
        let mut args = Vec::new();
        if let Some(command) = command {
            args.push(command);
        }
        args.push("--help");
        let output = Command::new(env!("CARGO_BIN_EXE_rawsmith")).args(args).output().unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        for phrase in ["--quality", "92", "smaller files"] {
            assert!(help.contains(phrase), "{help}");
        }
        if command != Some("compare") {
            for phrase in ["--max-bytes", "1000", "--size", "targets"] {
                assert!(help.contains(phrase), "{help}");
            }
        }
    }
}

#[test]
fn byte_limit_handles_non_monotonic_tiny_jpegs() {
    let f = Fixture::new("export-tiny-limit");
    let raw = rawsmith::arw::decode(&std::fs::read(&f.raw).unwrap()).unwrap();
    let rendered = rawsmith::pipeline::render(
        &raw,
        &rawsmith::params::Params::default(),
        rawsmith::pipeline::Size { max_edge: Some(1) },
    )
    .unwrap();
    let path = f.path("tiny.jpg");
    let encodings: Vec<_> = (60..=100).map(|q| rawsmith::output::encode(&path, &rendered.image, q).unwrap()).collect();
    let mut limits: Vec<_> = encodings.iter().map(Vec::len).collect();
    limits.sort_unstable();
    limits.dedup();
    for limit in limits {
        let expected = encodings.iter().rposition(|bytes| bytes.len() <= limit).unwrap();
        let report = ok(&[
            "render",
            s(&f.raw),
            "-o",
            s(&path),
            "--size",
            "1",
            "--quality",
            "100",
            "--max-bytes",
            &limit.to_string(),
        ]);
        assert_eq!(report["quality"], 60 + expected);
        assert_eq!(std::fs::read(&path).unwrap(), encodings[expected]);
    }
    let smallest = encodings.iter().map(Vec::len).min().unwrap();
    let (_, report) = run(&[
        "render",
        s(&f.raw),
        "-o",
        s(&path),
        "--size",
        "1",
        "--quality",
        "100",
        "--max-bytes",
        &(smallest - 1).to_string(),
    ]);
    assert!(report["error"]["message"].as_str().unwrap().contains(&format!("{smallest} bytes")));
}

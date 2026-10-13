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

#[test]
fn inspect_reports_camera_and_exposure() {
    let f = Fixture::new("inspect");
    let v = ok(&["inspect", s(&f.raw)]);
    assert_eq!(v["camera"]["model"], "ILCE-7M4");
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
    let v = ok(&["render", s(&f.raw), "--region", "0.25,0.25,0.75,0.75", "-o", s(&f.path("r.png"))]);
    assert_eq!((v["stats"]["width"].as_u64(), v["stats"]["height"].as_u64()), (Some(160), Some(120)));
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
    for expected in ["exposure", "temperature", "hsl.aqua.luminance", "grade.balance", "crop.right"] {
        assert!(paths.contains(&expected), "{expected} missing");
    }
    assert!(v["parameters"].as_array().unwrap().iter().all(|p| p["description"].as_str().unwrap().len() > 15));
}

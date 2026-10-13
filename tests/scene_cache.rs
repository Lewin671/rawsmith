//! Reusing the developed scene between invocations must never change a
//! picture: every output is compared with the same command run without it.

use std::path::{Path, PathBuf};
use std::process::Command;

use rawsmith::synthetic::Synthetic;
use serde_json::Value;

struct Fixture {
    dir: PathBuf,
    raw: PathBuf,
}

impl Fixture {
    /// Just over a megapixel: large enough for reuse, small enough to be quick.
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rawsmith-scene-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (1280, 800);
        let file = Synthetic::from_camera_rgb(w, h, |x, y| {
            let level = 0.02 + 0.5 * (x as f32 / w as f32) * (0.4 + 0.6 * ((y / 7 + x / 11) % 5) as f32 / 4.0);
            [level / 2.54, level, level / 1.56]
        });
        let raw = dir.join("frame.ARW");
        std::fs::write(&raw, file.to_bytes()).unwrap();
        Fixture { dir, raw }
    }

    fn path(&self, name: &str) -> String {
        self.dir.join(name).to_str().unwrap().to_owned()
    }

    fn run(&self, cache: Option<&Path>, args: &[&str]) -> Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rawsmith"));
        command.args(args).env_remove("RAWSMITH_CACHE_DIR").env_remove("RAWSMITH_CACHE_MB");
        match cache {
            Some(dir) => command.env("RAWSMITH_CACHE_DIR", dir),
            None => command.env("RAWSMITH_CACHE_MB", "0"),
        };
        let out = command.output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn scenes(&self, cache: &Path) -> usize {
        std::fs::read_dir(cache).map_or(0, |entries| {
            entries.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "scene")).count()
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn same_report(mut a: Value, mut b: Value) {
    for report in [&mut a, &mut b] {
        for key in ["elapsed_ms", "scene_reuse", "output", "recipe_file"] {
            report[key] = Value::Null;
        }
    }
    assert_eq!(a, b);
}

#[test]
fn reused_scenes_render_the_same_bytes_and_reports() {
    let f = Fixture::new("bytes");
    let cache = f.dir.join("cache");
    let raw = f.raw.to_str().unwrap();
    let recipe = f.path("recipe.json");
    std::fs::write(&recipe, r#"{"exposure":0.3,"shadows":20,"vibrance":15}"#).unwrap();
    // Finishing, framing, regions and formats all start from the stored scene.
    let cases: [&[&str]; 5] = [
        &["--size", "400"],
        &["--set", "crop.top=0.2", "--set", "rotate=3", "--set", "contrast=20"],
        &["--region", "0.2,0.2,0.5,0.6"],
        &["--set", r#"masks=[{"shape":"radial","center":[0.5,0.5],"radii":[0.2,0.2],"exposure":0.5}]"#],
        &["--set", "curve=[[0.25,0.2],[0.75,0.8]]", "--quality", "80"],
    ];
    for (index, extra) in cases.iter().enumerate() {
        for extension in ["jpg", "png"] {
            let (plain, cached) =
                (f.path(&format!("plain{index}.{extension}")), f.path(&format!("cached{index}.{extension}")));
            let args = |out: &str| {
                let mut args = vec!["render", raw, "--recipe", recipe.as_str(), "-o", out];
                args.extend(extra.iter().filter(|a| extension == "jpg" || !matches!(**a, "--quality" | "80")));
                args.iter().map(|a| a.to_string()).collect::<Vec<_>>()
            };
            let run = |cache: Option<&Path>, out: &str| {
                let owned = args(out);
                f.run(cache, &owned.iter().map(String::as_str).collect::<Vec<_>>())
            };
            let reference = run(None, &plain);
            assert_eq!(reference["scene_reuse"], "off");
            let with_cache = run(Some(&cache), &cached);
            assert_eq!(with_cache["scene_reuse"], if index == 0 && extension == "jpg" { "stored" } else { "reused" });
            assert_eq!(std::fs::read(&plain).unwrap(), std::fs::read(&cached).unwrap(), "case {index} {extension}");
            same_report(reference, with_cache);
        }
    }
    assert_eq!(f.scenes(&cache), 1);
}

#[test]
fn development_settings_and_sensor_data_select_different_scenes() {
    let f = Fixture::new("keys");
    let cache = f.dir.join("cache");
    let raw = f.raw.to_str().unwrap();
    let out = f.path("out.jpg");
    let render = |raw: &str, extra: &[&str]| {
        let mut args = vec!["render", raw, "-o", out.as_str(), "--size", "300"];
        args.extend(extra);
        f.run(Some(&cache), &args)["scene_reuse"].as_str().unwrap().to_owned()
    };
    assert_eq!(render(raw, &[]), "stored");
    assert_eq!(render(raw, &["--set", "exposure=1", "--set", "dehaze=20"]), "reused");
    // Each of these changes native development.
    for (count, setting) in ["denoise=40", "defringe=30", "temperature=4000", "tint=20"].iter().enumerate() {
        assert_eq!(render(raw, &["--set", setting]), "stored", "{setting}");
        assert_eq!(render(raw, &["--set", setting, "--set", "contrast=10"]), "reused", "{setting}");
        assert_eq!(f.scenes(&cache), count + 2);
    }
    // This file stores no lens tables, so switching corrections off develops the same scene.
    assert_eq!(render(raw, &["--set", "lens_corrections=false"]), "reused");
    // One changed sensor sample is a different photo.
    let mut bytes = std::fs::read(&f.raw).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let other = f.path("other.ARW");
    std::fs::write(&other, bytes).unwrap();
    assert_eq!(render(&other, &[]), "stored");
    assert_eq!(render(raw, &[]), "reused");
}

#[test]
fn a_damaged_scene_is_developed_again_and_disabling_writes_nothing() {
    let f = Fixture::new("damaged");
    let cache = f.dir.join("cache");
    let raw = f.raw.to_str().unwrap();
    let (first, second, plain) = (f.path("first.jpg"), f.path("second.jpg"), f.path("plain.jpg"));
    assert_eq!(f.run(None, &["render", raw, "-o", &plain])["scene_reuse"], "off");
    assert!(!cache.exists(), "a disabled cache must not create its directory");
    assert_eq!(f.run(Some(&cache), &["render", raw, "-o", &first])["scene_reuse"], "stored");
    let scene = std::fs::read_dir(&cache).unwrap().flatten().next().unwrap().path();
    let mut bytes = std::fs::read(&scene).unwrap();
    bytes.truncate(bytes.len() / 2);
    std::fs::write(&scene, bytes).unwrap();
    assert_eq!(f.run(Some(&cache), &["render", raw, "-o", &second])["scene_reuse"], "stored");
    for out in [&first, &second] {
        assert_eq!(std::fs::read(out).unwrap(), std::fs::read(&plain).unwrap());
    }
    // An unwritable cache location is not an error either.
    let blocked = f.path("not-a-directory");
    std::fs::write(&blocked, b"file").unwrap();
    assert_eq!(f.run(Some(Path::new(&blocked)), &["render", raw, "-o", &second])["scene_reuse"], "off");
    assert_eq!(std::fs::read(&second).unwrap(), std::fs::read(&plain).unwrap());
}

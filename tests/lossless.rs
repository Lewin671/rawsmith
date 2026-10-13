//! CLI acceptance of synthetic Sony Bayer lossless ARW and safe failures.
use rawsmith::{synthetic::Synthetic, tiff::Tiff};
use std::process::Command;

#[test]
fn lossless_inspect_render_and_malformed_json_preserve_files() {
    let dir = std::env::temp_dir().join(format!("rawsmith-lossless-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("lossless.ARW");
    let plain = dir.join("plain.ARW");
    let output = dir.join("lossless.png");
    let plain_output = dir.join("plain.png");
    let recipe = dir.join("lossless.ARW.rawsmith.json");
    let file = Synthetic::from_camera_rgb(64, 48, |x, y| {
        let v = (x + y) as f32 / 300.0;
        [v / 2.5, v, v / 1.6]
    });
    let original = file.to_lossless_bytes(32, 16, 1);
    std::fs::write(&input, &original).unwrap();
    std::fs::write(&plain, file.to_bytes()).unwrap();
    let invoke = |args: &[&std::ffi::OsStr]| Command::new(env!("CARGO_BIN_EXE_rawsmith")).args(args).output().unwrap();
    let inspect = invoke(&["inspect".as_ref(), input.as_os_str()]);
    assert!(inspect.status.success(), "{:?}", inspect.stderr);
    assert!(inspect.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(json["compression"], "lossless-compressed");
    for (raw, out) in [(&input, &output), (&plain, &plain_output)] {
        let result = invoke(&["render".as_ref(), raw.as_os_str(), "-o".as_ref(), out.as_os_str()]);
        assert!(result.status.success(), "{:?}", result.stderr);
    }
    let saved_output = std::fs::read(&output).unwrap();
    assert_eq!(saved_output, std::fs::read(&plain_output).unwrap());
    std::fs::write(&recipe, b"{}").unwrap();

    let (tiff, first) = Tiff::new(&original).unwrap();
    let ifd0 = tiff.ifd(first).unwrap();
    let raw = tiff.ifd(tiff.int(&ifd0, 0x014a).unwrap() as u32).unwrap();
    let patch = |tag: u16, value: u32| {
        let mut bytes = original.clone();
        let at = raw.entries[&tag].offset;
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        bytes
    };
    let mut bad_table = original.clone();
    let tile = tiff.int(&raw, 0x0144).unwrap() as usize;
    // SOI + 22-byte SOF3 precedes DHT. Replace its first length count.
    bad_table[tile + 29] = 255;
    for bytes in [
        original[..original.len() - 1].to_vec(),
        patch(0x0144, u32::MAX),
        patch(0x0100, 65535),
        bad_table,
        patch(0x0106, 6),
    ] {
        std::fs::write(&input, &bytes).unwrap();
        for command in ["inspect", "render", "edit"] {
            let result = if command == "inspect" {
                invoke(&[command.as_ref(), input.as_os_str()])
            } else {
                invoke(&[command.as_ref(), input.as_os_str(), "-o".as_ref(), output.as_os_str()])
            };
            assert_eq!(result.status.code(), Some(1));
            assert!(result.stdout.is_empty());
            let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
            assert!(matches!(error["error"]["kind"].as_str(), Some("format" | "unsupported")), "{error}");
            assert_eq!(std::fs::read(&input).unwrap(), bytes);
            assert_eq!(std::fs::read(&output).unwrap(), saved_output);
            assert_eq!(std::fs::read(&recipe).unwrap(), b"{}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

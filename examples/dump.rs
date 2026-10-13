//! Development aid: writes a RAW file's decoded sensor samples to stdout as
//! a one-line JSON header followed by little-endian u16 values, so they can be
//! compared with another decoder.
use std::io::Write;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump <file.ARW>");
    let raw = match rawsmith::arw::decode(&std::fs::read(&path).expect("cannot read file")) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "width": raw.width, "height": raw.height, "black": raw.black,
            "white": raw.white, "wb": raw.wb, "crop": raw.crop,
            "cfa": raw.cfa, "compression": raw.compression,
            "orientation": raw.meta.orientation,
        })
    )
    .unwrap();
    let bytes: Vec<u8> = raw.data.iter().flat_map(|v| v.to_le_bytes()).collect();
    out.write_all(&bytes).unwrap();
}

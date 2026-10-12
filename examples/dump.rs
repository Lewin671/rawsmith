//! Development aid: writes a RAW file's decoded sensor samples to stdout as
//! a one-line header followed by little-endian u16 values, so they can be
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
    writeln!(out, "{} {} {} {} {:.4} {:.4}", raw.width, raw.height, raw.black[0], raw.white, raw.wb[0], raw.wb[2])
        .unwrap();
    let bytes: Vec<u8> = raw.data.iter().flat_map(|v| v.to_le_bytes()).collect();
    out.write_all(&bytes).unwrap();
}

//! Sony ARW reader for uncompressed raw data.
//!
//! An ARW is a TIFF container. IFD0 carries camera metadata and a preview
//! JPEG, its SubIFD holds the sensor mosaic together with the levels, white
//! balance and lens-correction tables this module extracts.

use serde::Serialize;

use crate::error::{Error, Result};
use crate::tiff::{Ifd, Tiff};

const TAG_SUBIFD: u16 = 0x014a;
const TAG_EXIF: u16 = 0x8769;
const SONY_RAW_COMPRESSED: i64 = 32767;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Metadata {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub datetime: Option<String>,
    pub iso: Option<u32>,
    pub shutter_seconds: Option<f64>,
    pub aperture: Option<f64>,
    pub focal_length_mm: Option<f64>,
    /// EXIF orientation (1 = upright, 3 = 180, 6 = rotate 90 CW, 8 = 90 CCW).
    pub orientation: u16,
}

/// Radial lens-correction tables the camera stores next to the mosaic. Each
/// table samples the image radius at `n` evenly spaced knots, knot `i` lying
/// at `(i + 0.5) / (n - 1)` of the half diagonal.
#[derive(Debug, Clone, Default)]
pub struct LensTables {
    pub distortion: Vec<i16>,
    pub ca_red: Vec<i16>,
    pub ca_blue: Vec<i16>,
    pub vignetting: Vec<i16>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Crop {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

pub struct RawImage {
    /// Full sensor mosaic, `width * height` samples.
    pub data: Vec<u16>,
    pub width: usize,
    pub height: usize,
    /// Colour of each cell of the 2x2 CFA tile: 0 = red, 1 = green, 2 = blue.
    pub cfa: [u8; 4],
    /// Black level per CFA tile cell.
    pub black: [f32; 4],
    pub white: f32,
    /// As-shot white balance multipliers for R, G, B (G = 1).
    pub wb: [f32; 3],
    /// Area of the mosaic that forms the picture.
    pub crop: Crop,
    pub lens: Option<LensTables>,
    pub meta: Metadata,
    /// Byte range of the largest embedded JPEG, if any.
    pub embedded_jpeg: Option<(usize, usize)>,
}

fn i16s(tiff: &Tiff, ifd: &Ifd, tag: u16) -> Option<Vec<i16>> {
    Some(tiff.ints(ifd, tag, 64)?.into_iter().map(|v| v as i16).collect())
}

/// Sony prefixes each table with its element count.
fn counted(table: Option<Vec<i16>>) -> Option<Vec<i16>> {
    let table = table?;
    let n = *table.first()? as usize;
    (n >= 2 && table.len() > n).then(|| table[1..=n].to_vec())
}

fn lens_tables(tiff: &Tiff, raw: &Ifd) -> Option<LensTables> {
    let distortion = counted(i16s(tiff, raw, 0x7037))?;
    let n = distortion.len();
    let ca = counted(i16s(tiff, raw, 0x7035)).filter(|t| t.len() == 2 * n);
    let vignetting = counted(i16s(tiff, raw, 0x7032)).filter(|t| t.len() == n);
    Some(LensTables {
        ca_red: ca.as_ref().map(|t| t[..n].to_vec()).unwrap_or_else(|| vec![0; n]),
        ca_blue: ca.map(|t| t[n..].to_vec()).unwrap_or_else(|| vec![0; n]),
        vignetting: vignetting.unwrap_or_else(|| vec![0; n]),
        distortion,
    })
}

fn metadata(tiff: &Tiff, ifd0: &Ifd) -> Metadata {
    let mut meta = Metadata {
        make: tiff.string(ifd0, 0x010f),
        model: tiff.string(ifd0, 0x0110),
        orientation: tiff.int(ifd0, 0x0112).unwrap_or(1) as u16,
        ..Metadata::default()
    };
    let exif = tiff.int(ifd0, TAG_EXIF).and_then(|at| tiff.ifd(at as u32).ok());
    if let Some(exif) = exif {
        meta.shutter_seconds = tiff.float(&exif, 0x829a);
        meta.aperture = tiff.float(&exif, 0x829d);
        meta.iso = tiff.int(&exif, 0x8827).map(|v| v as u32);
        meta.datetime = tiff.string(&exif, 0x9003);
        meta.focal_length_mm = tiff.float(&exif, 0x920a);
        meta.lens = tiff.string(&exif, 0xa434);
    }
    meta
}

/// Finds the largest JPEG stream referenced from the top-level IFD chain.
fn embedded_jpeg(tiff: &Tiff, first: u32) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut at = first;
    for _ in 0..8 {
        if at == 0 {
            break;
        }
        let Ok(ifd) = tiff.ifd(at) else { break };
        if let (Some(offset), Some(len)) = (tiff.int(&ifd, 0x0201), tiff.int(&ifd, 0x0202)) {
            let (offset, len) = (offset as usize, len as usize);
            let valid = offset.checked_add(len).is_some_and(|end| end <= tiff.data.len());
            if valid && best.map_or(true, |(_, l)| len > l) {
                best = Some((offset, len));
            }
        }
        at = ifd.next;
    }
    best
}

pub fn decode(file: &[u8]) -> Result<RawImage> {
    let (tiff, first) = Tiff::new(file)?;
    let ifd0 = tiff.ifd(first)?;
    let meta = metadata(&tiff, &ifd0);

    // The mosaic lives in a SubIFD with a CFA photometric interpretation.
    let raw = tiff
        .ints(&ifd0, TAG_SUBIFD, 8)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|at| tiff.ifd(at as u32).ok())
        .find(|ifd| tiff.int(ifd, 0x0106) == Some(32803))
        .ok_or_else(|| Error::Format("no raw image data found in this file".into()))?;

    let need = |tag: u16, name: &str| {
        tiff.int(&raw, tag).ok_or_else(|| Error::Format(format!("raw image is missing {name}")))
    };
    let width = need(0x0100, "its width")? as usize;
    let height = need(0x0101, "its height")? as usize;
    let compression = need(0x0103, "its compression type")?;
    let bits = need(0x0102, "its bit depth")?;
    if compression == SONY_RAW_COMPRESSED {
        return Err(Error::Unsupported(
            "compressed ARW is not supported yet; shoot or convert to uncompressed RAW".into(),
        ));
    }
    if compression != 1 || !(12..=16).contains(&bits) {
        return Err(Error::Unsupported(format!(
            "raw encoding (compression {compression}, {bits} bits) is not supported"
        )));
    }
    let samples = width
        .checked_mul(height)
        .filter(|&n| n > 0 && n <= 400_000_000)
        .ok_or_else(|| Error::Format(format!("implausible raw size {width}x{height}")))?;
    let strip = need(0x0111, "its data offset")? as usize;
    let bytes = strip
        .checked_add(samples * 2)
        .and_then(|end| file.get(strip..end))
        .ok_or_else(|| Error::Format("raw data extends past the end of the file".into()))?;
    let data: Vec<u16> = bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();

    let cfa = match tiff.ints(&raw, 0x828e, 4) {
        Some(p) if p.len() == 4 && p.iter().all(|&c| (0..=2).contains(&c)) => {
            [p[0] as u8, p[1] as u8, p[2] as u8, p[3] as u8]
        }
        _ => return Err(Error::Unsupported("only 2x2 RGB Bayer sensors are supported".into())),
    };

    let white = tiff.int(&raw, 0xc61d).unwrap_or((1 << bits) - 1) as f32;
    let black = match tiff.ints(&raw, 0x7310, 4) {
        Some(b) if b.len() == 4 => [b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32],
        _ => [white / 32.0; 4],
    };
    // WB_RGGBLevels; fall back to a daylight-like guess when absent.
    let wb = match tiff.ints(&raw, 0x7313, 4) {
        Some(l) if l.len() == 4 && l.iter().all(|&v| v > 0) => {
            let g = (l[1] + l[2]) as f32 / 2.0;
            [l[0] as f32 / g, 1.0, l[3] as f32 / g]
        }
        _ => [2.5, 1.0, 1.6],
    };

    let full = Crop { x: 0, y: 0, width, height };
    let crop = match (tiff.ints(&raw, 0xc61f, 2), tiff.ints(&raw, 0xc620, 2)) {
        (Some(o), Some(s)) if o.len() == 2 && s.len() == 2 => {
            let c = Crop { x: o[0] as usize, y: o[1] as usize, width: s[0] as usize, height: s[1] as usize };
            let fits = c.width > 0 && c.height > 0 && c.x + c.width <= width && c.y + c.height <= height;
            // Keep the Bayer phase: an odd origin would shift the CFA tile.
            if fits && c.x % 2 == 0 && c.y % 2 == 0 { c } else { full }
        }
        _ => full,
    };

    Ok(RawImage {
        data,
        width,
        height,
        cfa,
        black,
        white,
        wb,
        crop,
        lens: lens_tables(&tiff, &raw),
        meta,
        embedded_jpeg: embedded_jpeg(&tiff, first),
    })
}

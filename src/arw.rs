//! Sony ARW reader for uncompressed, lossy and lossless-compressed Bayer data.
//!
//! An ARW is a TIFF container. IFD0 carries camera metadata and a preview
//! JPEG, its SubIFD holds the sensor mosaic together with the levels, white
//! balance and lens-correction tables this module extracts.

mod ljpeg;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::tiff::{Ifd, Tiff};

const TAG_SUBIFD: u16 = 0x014a;
const TAG_EXIF: u16 = 0x8769;
const SONY_RAW_COMPRESSED: i64 = 32767;
/// Smallest picture edge the pipeline's neighbourhood filters can handle.
const MIN_EDGE: usize = 16;

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
    /// Encoding of the sensor samples, not of the embedded JPEG.
    pub compression: Compression,
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

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Compression {
    Uncompressed,
    LossyCompressed,
    LosslessCompressed,
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

/// Sony's lossy compression stores 11-bit values through a piecewise-linear
/// curve whose four knee points are in the file. Returns the lookup table
/// from stored value to linear sensor value.
fn tone_curve(points: &[i64]) -> Vec<u16> {
    let mut knees = [0usize, 0, 0, 0, 0, 4095];
    for (knee, &p) in knees[1..5].iter_mut().zip(points) {
        *knee = ((p >> 2) & 0xfff) as usize;
    }
    let mut curve: Vec<u16> = (0..=0x4000u32).map(|v| v.min(0x3fff) as u16).collect();
    for i in 0..5 {
        for j in knees[i] + 1..=knees[i + 1].min(4095) {
            curve[j] = curve[j - 1].saturating_add(1 << i);
        }
    }
    curve
}

/// Decodes Sony's lossy "compressed RAW": each row is split into 32-pixel
/// groups, two interleaved colours of 16 samples each. A 16-byte block holds
/// one colour's 11-bit maximum and minimum, their positions, and the other
/// 14 samples as 7-bit offsets from the minimum, scaled to span the range.
fn decode_compressed(bytes: &[u8], width: usize, curve: &[u16]) -> Vec<u16> {
    let mut out = vec![0u16; bytes.len()];
    for (row, packed) in out.chunks_exact_mut(width).zip(bytes.chunks_exact(width)) {
        for (block, chunk) in packed.as_chunks::<16>().0.iter().enumerate() {
            let bits = u128::from_le_bytes(*chunk);
            // A damaged block can ask for more fields than it holds; those read as zero.
            let field = |at: u32, len: u32| (bits.checked_shr(at).unwrap_or(0) & ((1 << len) - 1)) as u32;
            let (max, min) = (field(0, 11), field(11, 11));
            let (at_max, at_min) = (field(22, 4), field(26, 4));
            let mut shift = 0;
            while shift < 4 && (0x80 << shift) <= max.saturating_sub(min) {
                shift += 1;
            }
            let mut at = 30;
            // Blocks alternate between the row's two colours.
            let first = (block / 2) * 32 + block % 2;
            for i in 0..16 {
                let value = if i == at_max {
                    max
                } else if i == at_min {
                    min
                } else {
                    let v = ((field(at, 7) << shift) + min).min(0x7ff);
                    at += 7;
                    v
                };
                row[first + i as usize * 2] = curve[(value << 1) as usize];
            }
        }
    }
    out
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
            if valid && best.is_none_or(|(_, l)| len > l) {
                best = Some((offset, len));
            }
        }
        at = ifd.next;
    }
    best
}

/// Sony's four JPEG components are the cells of a 2x2 Bayer block. TIFF
/// tile arrays are row-major even though payloads are stored column-major.
fn decode_lossless(tiff: &Tiff, raw: &Ifd, width: usize, height: usize, bits: u8) -> Result<Vec<u16>> {
    let need = |tag, name| tiff.int(raw, tag).ok_or_else(|| Error::Format(format!("missing lossless {name}")));
    let tw = need(0x0142, "tile width")?;
    let th = need(0x0143, "tile height")?;
    if !(2..=512).contains(&tw) || !(2..=512).contains(&th) || tw % 2 != 0 || th % 2 != 0 {
        return Err(Error::Format("invalid lossless tile dimensions".into()));
    }
    // This camera's largest padded frame is 7168x5120. Bound memory and
    // decoding work independently of dimensions claimed by an untrusted IFD.
    if width > 8192
        || height > 8192
        || width * height > 40_000_000
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
    {
        return Err(Error::Format("implausible lossless sensor dimensions".into()));
    }
    let (tw, th) = (tw as usize, th as usize);
    let (cols, rows) = (width.div_ceil(tw), height.div_ceil(th));
    let count = cols * rows;
    if count > 4096 || count * tw * th > 50_000_000 {
        return Err(Error::Format("implausible lossless tile count".into()));
    }
    let array = |tag, name| {
        if tiff.count(raw, tag) != Some(count) {
            return Err(Error::Format(format!("invalid lossless {name} count")));
        }
        tiff.ints(raw, tag, count).ok_or_else(|| Error::Format(format!("missing lossless {name}")))
    };
    let offsets = array(0x0144, "tile offsets")?;
    let sizes = array(0x0145, "tile byte counts")?;
    let mut ranges = Vec::with_capacity(count);
    for (&offset, &size) in offsets.iter().zip(&sizes) {
        if !(1..=2_000_000).contains(&size) {
            return Err(Error::Format("implausible lossless tile byte count".into()));
        }
        let range = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(size).ok())
            .and_then(|(start, len)| {
                start.checked_add(len).filter(|&end| end <= tiff.data.len()).map(|end| (start, end))
            })
            .ok_or_else(|| Error::Format("lossless tile extends past the end of the file".into()))?;
        ranges.push(range);
    }
    let mut ordered = ranges.clone();
    ordered.sort_unstable();
    if ordered.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(Error::Format("overlapping lossless tiles".into()));
    }
    // Validate every header before allocating the sensor mosaic, retaining
    // only ranges rather than thousands of tables with separate allocations.
    for &(start, end) in &ranges {
        ljpeg::Tile::parse(&tiff.data[start..end], tw / 2, th / 2, bits)?;
    }
    let mut data = vec![0u16; width * height];
    for (i, &(start, end)) in ranges.iter().enumerate() {
        let values = ljpeg::Tile::parse(&tiff.data[start..end], tw / 2, th / 2, bits)?.decode()?;
        let (left, top) = (i % cols * tw, i / cols * th);
        for y in 0..th.min(height - top) {
            for x in 0..tw.min(width - left) {
                let component = (y % 2) * 2 + x % 2;
                data[(top + y) * width + left + x] = values[((y / 2) * (tw / 2) + x / 2) * 4 + component];
            }
        }
    }
    Ok(data)
}

pub fn decode(file: &[u8]) -> Result<RawImage> {
    let _stage = crate::profile::Stage::new("decode");
    let (tiff, first) = Tiff::new(file)?;
    let ifd0 = tiff.ifd(first)?;
    let meta = metadata(&tiff, &ifd0);

    // Sony stores Bayer and reduced-size YCbCr RAWs in a SubIFD.
    let raw = tiff
        .ints(&ifd0, TAG_SUBIFD, 8)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|at| tiff.ifd(at as u32).ok())
        .find(|ifd| matches!(tiff.int(ifd, 0x0106), Some(32803 | 6)))
        .ok_or_else(|| Error::Format("no raw image data found in this file".into()))?;

    if tiff.int(&raw, 0x0106) == Some(6) {
        return Err(Error::Unsupported(
            "reduced-size Sony YCbCr RAW is not supported; lossless Bayer L and APS-C M are supported".into(),
        ));
    }

    let need =
        |tag: u16, name: &str| tiff.int(&raw, tag).ok_or_else(|| Error::Format(format!("raw image is missing {name}")));
    let width = need(0x0100, "its width")? as usize;
    let height = need(0x0101, "its height")? as usize;
    let compression = need(0x0103, "its compression type")?;
    let bits = need(0x0102, "its bit depth")?;
    if !(8..=16).contains(&bits) {
        return Err(Error::Unsupported(format!("{bits}-bit raw data is not supported")));
    }
    let samples = width
        .checked_mul(height)
        .filter(|&n| n > 0 && n <= 400_000_000)
        .ok_or_else(|| Error::Format(format!("implausible raw size {width}x{height}")))?;
    let strip = tiff.int(&raw, 0x0111).unwrap_or(-1) as usize;
    let strip_bytes = |len: usize| {
        strip
            .checked_add(len)
            .and_then(|end| file.get(strip..end))
            .ok_or_else(|| Error::Format("raw data extends past the end of the file".into()))
    };
    let data: Vec<u16> = match compression {
        7 => decode_lossless(&tiff, &raw, width, height, bits as u8)?,
        1 if (12..=16).contains(&bits) => {
            strip_bytes(samples * 2)?.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect()
        }
        SONY_RAW_COMPRESSED if width.is_multiple_of(32) => {
            let curve = tone_curve(&tiff.ints(&raw, 0x7010, 4).unwrap_or_default());
            decode_compressed(strip_bytes(samples)?, width, &curve)
        }
        _ => {
            return Err(Error::Unsupported(format!(
                "raw encoding (compression {compression}, {bits} bits, width {width}) is not supported; \
                 uncompressed, lossy-compressed and lossless-compressed Bayer ARW are"
            )))
        }
    };

    // Only true Bayer tiles: green on one diagonal, red and blue on the other.
    let cfa = match tiff.ints(&raw, 0x828e, 4).as_deref() {
        Some([0, 1, 1, 2]) => [0, 1, 1, 2],
        Some([2, 1, 1, 0]) => [2, 1, 1, 0],
        Some([1, 0, 2, 1]) => [1, 0, 2, 1],
        Some([1, 2, 0, 1]) => [1, 2, 0, 1],
        _ => return Err(Error::Unsupported("only 2x2 RGB Bayer sensors are supported".into())),
    };

    let white = tiff.int(&raw, 0xc61d).filter(|&w| w > 0).unwrap_or((1i64 << bits) - 1) as f32;
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

    // The picture area must lie inside the sensor, start on a whole CFA
    // tile (so the Bayer phase is unchanged) and cover whole tiles.
    let full = Crop { x: 0, y: 0, width: width & !1, height: height & !1 };
    let crop = match (tiff.ints(&raw, 0xc61f, 2).as_deref(), tiff.ints(&raw, 0xc620, 2).as_deref()) {
        (Some(&[x, y]), Some(&[w, h])) => {
            let inside = x >= 0 && y >= 0 && w > 0 && h > 0 && x + w <= width as i64 && y + h <= height as i64;
            if inside && x % 2 == 0 && y % 2 == 0 {
                Crop { x: x as usize, y: y as usize, width: w as usize & !1, height: h as usize & !1 }
            } else {
                full
            }
        }
        _ => full,
    };
    if crop.width < MIN_EDGE || crop.height < MIN_EDGE {
        return Err(Error::Format(format!("raw image of {width}x{height} is too small to develop")));
    }

    Ok(RawImage {
        compression: match compression {
            1 => Compression::Uncompressed,
            7 => Compression::LosslessCompressed,
            _ => Compression::LossyCompressed,
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sony_counted_lens_tables_keep_signed_red_and_blue_values() {
        use crate::tiff::Entry;
        let mut bytes = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
        let mut ifd = Ifd::default();
        let tables = [
            (0x7037, vec![4, 0, 10, 20, 30]),
            (0x7035, vec![8, -128, -256, -512, -1024, 128, 256, 512, 1024]),
            (0x7032, vec![4, 0, 100, 200, 300]),
        ];
        for (tag, values) in tables {
            let offset = bytes.len();
            let count = values.len() as u32;
            for v in values {
                bytes.extend((v as i16).to_le_bytes());
            }
            ifd.entries.insert(tag, Entry { kind: 8, count, offset });
        }
        let (tiff, _) = Tiff::new(&bytes).unwrap();
        let lens = lens_tables(&tiff, &ifd).unwrap();
        assert_eq!(lens.ca_red, [-128, -256, -512, -1024]);
        assert_eq!(lens.ca_blue, [128, 256, 512, 1024]);
        assert_eq!(lens.distortion, [0, 10, 20, 30]);
        assert_eq!(lens.vignetting, [0, 100, 200, 300]);
    }

    #[test]
    fn compressed_block_decodes_extremes_and_scaled_offsets() {
        // One colour's block: max 1000 at position 3, min 200 at position 0,
        // and fourteen 7-bit offsets 1..=14. The range 800 needs a shift of 3.
        let mut bits: u128 = 1000 | (200 << 11) | (3 << 22);
        for k in 0..14u128 {
            bits |= (k + 1) << (30 + 7 * k);
        }
        let mut row = vec![0u8; 32];
        row[..16].copy_from_slice(&bits.to_le_bytes());
        row[16..].copy_from_slice(&bits.to_le_bytes());
        let identity: Vec<u16> = (0..=0x4000).collect();
        let out = decode_compressed(&row, 32, &identity);
        // Stored values are 11-bit; the curve is indexed at twice the value.
        let expected = |i: usize| match i {
            0 => 200,
            3 => 1000,
            1 | 2 => 200 + (i << 3),
            _ => 200 + ((i - 1) << 3),
        };
        for i in 0..16 {
            assert_eq!(out[i * 2] as usize, expected(i) * 2, "first colour, sample {i}");
            assert_eq!(out[i * 2 + 1] as usize, expected(i) * 2, "second colour, sample {i}");
        }
    }

    #[test]
    fn damaged_compressed_blocks_decode_without_panicking() {
        let identity: Vec<u16> = (0..=0x4000).collect();
        // All zero: maximum and minimum share position 0, so fifteen offsets are requested.
        assert_eq!(decode_compressed(&[0u8; 32], 32, &identity), vec![0u16; 32]);
        decode_compressed(&[0xffu8; 64], 32, &identity);
    }

    #[test]
    fn tone_curve_is_monotone_and_compresses_highlights() {
        let curve = tone_curve(&[8000, 10400, 12900, 14100]);
        assert!(curve.windows(2).take(4095).all(|w| w[1] >= w[0]));
        // Below the first knee the curve is the identity; above, steps widen.
        assert_eq!(curve[1000], 1000);
        assert!(curve[4000] - curve[3999] > 1);
    }
}

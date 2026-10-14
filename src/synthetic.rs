//! Builds small, valid uncompressed and lossless Bayer ARW files from pixel data. Tests and the
//! public benchmark use these, so neither needs real photographs.

/// Description of a synthetic RAW file.
pub struct Synthetic {
    pub width: usize,
    pub height: usize,
    /// Sensor samples, `width * height`, already including the black level.
    pub data: Vec<u16>,
    pub model: String,
    pub orientation: u16,
    pub black: u16,
    pub white: u16,
    /// WB_RGGBLevels as the camera would store them.
    pub wb_levels: [i16; 4],
    /// Colour of the camera JPEG, declared in the RAW's ExifIFD (0xa001).
    pub exif_color_space: Option<u16>,
    /// NUL-terminated InteropIFD index (R98 = sRGB, R03 = Adobe RGB).
    pub interoperability_index: Option<[u8; 4]>,
    /// ExifIFD ISOSpeedRatings (0x8827).
    pub iso: Option<u16>,
    pub embedded_jpeg: Option<Vec<u8>>,
}

impl Synthetic {
    /// A file shaped like an A7 IV frame: RGGB, 14-bit, black level 512.
    pub fn new(width: usize, height: usize, data: Vec<u16>) -> Self {
        assert_eq!(data.len(), width * height);
        Synthetic {
            width,
            height,
            data,
            model: "ILCE-7M4".into(),
            orientation: 1,
            black: 512,
            white: 16383,
            wb_levels: [2600, 1024, 1024, 1600],
            exif_color_space: None,
            interoperability_index: None,
            iso: None,
            embedded_jpeg: None,
        }
    }

    /// Mosaics a linear camera-RGB image (values 0..1 of the sensor range,
    /// before white balance) into sensor samples.
    pub fn from_camera_rgb(width: usize, height: usize, pixel: impl Fn(usize, usize) -> [f32; 3]) -> Self {
        let mut data = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let channel = [0, 1, 1, 2][(y & 1) * 2 + (x & 1)];
                let v = pixel(x, y)[channel].clamp(0.0, 1.0);
                data.push((512.0 + v * (16383.0 - 512.0)).round() as u16);
            }
        }
        Synthetic::new(width, height, data)
    }

    /// Encodes four Bayer planes in tiled SOF3 JPEG, as Sony's lossless
    /// Bayer files do. Fixed five-bit Huffman codes deliberately differ from
    /// the camera's optimized tables; tests cover the format, not a fixture.
    pub fn to_lossless_bytes(&self, tile_width: usize, tile_height: usize, predictor: u8) -> Vec<u8> {
        assert!(tile_width > 0 && tile_height > 0 && tile_width.is_multiple_of(2) && tile_height.is_multiple_of(2));
        assert!((1..=7).contains(&predictor));
        let mut out = self.to_bytes();
        let (tiff, first) = crate::tiff::Tiff::new(&out).unwrap();
        let ifd0 = tiff.ifd(first).unwrap();
        let old = tiff.ifd(tiff.int(&ifd0, 0x014a).unwrap() as u32).unwrap();
        let pointer = ifd0.entries[&0x014a].offset;
        let mut entries: Vec<(u16, u16, u32, [u8; 4])> = old
            .entries
            .iter()
            .filter(|(&tag, _)| tag != 0x0111 && tag != 0x0117)
            .map(|(&tag, e)| {
                let size = match e.kind {
                    1 => 1,
                    4 => 4,
                    _ => 2,
                };
                let value = if e.count as usize * size <= 4 {
                    out[e.offset..e.offset + 4].try_into().unwrap()
                } else {
                    (e.offset as u32).to_le_bytes()
                };
                (tag, e.kind, e.count, value)
            })
            .collect();
        for e in &mut entries {
            if e.0 == 0x0103 {
                e.3 = 7u32.to_le_bytes();
            }
        }
        let cols = self.width.div_ceil(tile_width);
        let rows = self.height.div_ceil(tile_height);
        let count = cols * rows;
        let ifd_at = out.len();
        let arrays_at = ifd_at + 2 + (entries.len() + 4) * 12 + 4;
        let pixels_at = arrays_at + count * 8;
        let mut payload = Vec::new();
        let mut offsets = Vec::new();
        let mut sizes = Vec::new();
        for row in 0..rows {
            for col in 0..cols {
                let mut planes = Vec::new();
                for y in 0..tile_height / 2 {
                    for x in 0..tile_width / 2 {
                        for c in 0..4 {
                            let sx = col * tile_width + x * 2 + c % 2;
                            let sy = row * tile_height + y * 2 + c / 2;
                            planes.push(if sx < self.width && sy < self.height {
                                self.data[sy * self.width + sx]
                            } else {
                                0
                            });
                        }
                    }
                }
                let jpeg = lossless_jpeg(&planes, tile_width / 2, tile_height / 2, 14, predictor, 0);
                offsets.push((pixels_at + payload.len()) as u32);
                sizes.push(jpeg.len() as u32);
                payload.extend(jpeg);
            }
        }
        let array_value =
            |at: usize, values: &[u32]| if values.len() == 1 { values[0] } else { at as u32 }.to_le_bytes();
        entries.extend([
            (0x0142, 4, 1, (tile_width as u32).to_le_bytes()),
            (0x0143, 4, 1, (tile_height as u32).to_le_bytes()),
            (0x0144, 4, count as u32, array_value(arrays_at, &offsets)),
            (0x0145, 4, count as u32, array_value(arrays_at + count * 4, &sizes)),
        ]);
        out[pointer..pointer + 4].copy_from_slice(&(ifd_at as u32).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        for (tag, kind, count, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value);
        }
        out.extend(0u32.to_le_bytes());
        for value in offsets.iter().chain(&sizes) {
            out.extend(value.to_le_bytes());
        }
        out.extend(payload);
        out
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        // Layout: header, IFD0, strings, optional EXIF/Interop, raw IFD,
        // its arrays, pixel data, optional bare preview JPEG.
        let mut out = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        let model = format!("{}\0", self.model);
        let entry = |tag: u16, kind: u16, count: u32, value: u32| {
            let mut e = Vec::with_capacity(12);
            e.extend(tag.to_le_bytes());
            e.extend(kind.to_le_bytes());
            e.extend(count.to_le_bytes());
            e.extend(value.to_le_bytes());
            e
        };
        let has_exif = self.exif_color_space.is_some() || self.interoperability_index.is_some() || self.iso.is_some();
        let ifd0_entries = 4u32 + u32::from(has_exif) + 2 * u32::from(self.embedded_jpeg.is_some());
        let ifd0_end = 8 + 2 + ifd0_entries * 12 + 4;
        let make_at = ifd0_end;
        let model_at = make_at + 5;
        let exif_at = (model_at + model.len() as u32 + 1) & !1;
        let exif_entries = u32::from(self.iso.is_some())
            + u32::from(self.exif_color_space.is_some())
            + u32::from(self.interoperability_index.is_some());
        let interop_at = exif_at + 2 + exif_entries * 12 + 4;
        let raw_ifd_at =
            if has_exif { interop_at + if self.interoperability_index.is_some() { 18 } else { 0 } } else { exif_at };
        let raw_entries = 13u32;
        let arrays_at = raw_ifd_at + 2 + raw_entries * 12 + 4;
        let (black_at, wb_at, origin_at, size_at) = (arrays_at, arrays_at + 8, arrays_at + 16, arrays_at + 24);
        let strip_at = arrays_at + 32;

        out.extend((ifd0_entries as u16).to_le_bytes());
        out.extend(entry(0x010f, 2, 5, make_at));
        out.extend(entry(0x0110, 2, model.len() as u32, model_at));
        out.extend(entry(0x0112, 3, 1, self.orientation as u32));
        out.extend(entry(0x014a, 4, 1, raw_ifd_at));
        if let Some(jpeg) = &self.embedded_jpeg {
            out.extend(entry(0x0201, 4, 1, strip_at + (self.data.len() * 2) as u32));
            out.extend(entry(0x0202, 4, 1, jpeg.len() as u32));
        }
        if has_exif {
            out.extend(entry(0x8769, 4, 1, exif_at));
        }
        out.extend(0u32.to_le_bytes());
        out.extend(b"SONY\0");
        out.extend(model.as_bytes());
        out.resize(exif_at as usize, 0);
        if has_exif {
            out.extend((exif_entries as u16).to_le_bytes());
            if let Some(iso) = self.iso {
                out.extend(entry(0x8827, 3, 1, iso as u32));
            }
            if let Some(space) = self.exif_color_space {
                out.extend(entry(0xa001, 3, 1, space as u32));
            }
            if self.interoperability_index.is_some() {
                out.extend(entry(0xa005, 4, 1, interop_at));
            }
            out.extend(0u32.to_le_bytes());
            if let Some(index) = self.interoperability_index {
                out.extend(1u16.to_le_bytes());
                out.extend(entry(0x0001, 2, 4, u32::from_le_bytes(index)));
                out.extend(0u32.to_le_bytes());
            }
        }
        debug_assert_eq!(out.len(), raw_ifd_at as usize);

        out.extend((raw_entries as u16).to_le_bytes());
        out.extend(entry(0x0100, 3, 1, self.width as u32));
        out.extend(entry(0x0101, 3, 1, self.height as u32));
        out.extend(entry(0x0102, 3, 1, 14));
        out.extend(entry(0x0103, 3, 1, 1));
        out.extend(entry(0x0106, 3, 1, 32803));
        out.extend(entry(0x0111, 4, 1, strip_at));
        out.extend(entry(0x0117, 4, 1, (self.data.len() * 2) as u32));
        out.extend(entry(0x7310, 3, 4, black_at));
        out.extend(entry(0x7313, 8, 4, wb_at));
        out.extend(entry(0x828e, 1, 4, u32::from_le_bytes([0, 1, 1, 2])));
        out.extend(entry(0xc61d, 3, 1, self.white as u32));
        out.extend(entry(0xc61f, 4, 2, origin_at));
        out.extend(entry(0xc620, 4, 2, size_at));
        out.extend(0u32.to_le_bytes());
        for _ in 0..4 {
            out.extend(self.black.to_le_bytes());
        }
        for level in self.wb_levels {
            out.extend(level.to_le_bytes());
        }
        out.extend(0u32.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend((self.width as u32).to_le_bytes());
        out.extend((self.height as u32).to_le_bytes());
        debug_assert_eq!(out.len(), strip_at as usize);
        for v in &self.data {
            out.extend(v.to_le_bytes());
        }
        if let Some(jpeg) = &self.embedded_jpeg {
            out.extend(jpeg);
        }
        out
    }
}

/// A small independent encoder for the T.81 prediction/difference rules.
pub(crate) fn lossless_jpeg(
    data: &[u16],
    width: usize,
    height: usize,
    precision: u8,
    predictor: u8,
    transform: u8,
) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8];
    let segment = |out: &mut Vec<u8>, marker: u8, data: &[u8]| {
        out.extend([0xff, marker]);
        out.extend(((data.len() + 2) as u16).to_be_bytes());
        out.extend(data);
    };
    let mut frame = vec![precision];
    frame.extend((height as u16).to_be_bytes());
    frame.extend((width as u16).to_be_bytes());
    frame.push(4);
    for c in 1..=4 {
        frame.extend([c, 0x11, 0]);
    }
    segment(&mut out, 0xc3, &frame);
    let mut table = vec![0; 17];
    table[5] = 17;
    table.extend(0..=16);
    segment(&mut out, 0xc4, &table);
    segment(&mut out, 0xda, &[4, 1, 0, 2, 0, 3, 0, 4, 0, predictor, 0, transform]);
    let (mut byte, mut used) = (0u8, 0u8);
    let write = |out: &mut Vec<u8>, byte: &mut u8, used: &mut u8, value: u32, count: u8| {
        for shift in (0..count).rev() {
            *byte = (*byte << 1) | ((value >> shift) & 1) as u8;
            *used += 1;
            if *used == 8 {
                out.push(*byte);
                if *byte == 0xff {
                    out.push(0);
                }
                *byte = 0;
                *used = 0;
            }
        }
    };
    for y in 0..height {
        for x in 0..width {
            for c in 0..4 {
                let at = (y * width + x) * 4 + c;
                let sample = |i: usize| i32::from(data[i] >> transform);
                let prediction = if x == 0 && y == 0 {
                    1 << (precision - transform - 1)
                } else if y == 0 {
                    sample(at - 4)
                } else if x == 0 {
                    sample(at - width * 4)
                } else {
                    let (a, b, c) = (sample(at - 4), sample(at - width * 4), sample(at - width * 4 - 4));
                    match predictor {
                        1 => a,
                        2 => b,
                        3 => c,
                        4 => a + b - c,
                        5 => a + ((b - c) >> 1),
                        6 => b + ((a - c) >> 1),
                        7 => (a + b) >> 1,
                        _ => unreachable!(),
                    }
                };
                let difference = (sample(at) - prediction) as i16 as i32;
                let category =
                    if difference == -32768 { 16 } else { (32 - difference.unsigned_abs().leading_zeros()) as u8 };
                write(&mut out, &mut byte, &mut used, category as u32, 5);
                if category > 0 && category < 16 {
                    let value = if difference < 0 { difference + (1 << category) - 1 } else { difference };
                    write(&mut out, &mut byte, &mut used, value as u32, category);
                }
            }
        }
    }
    if used != 0 {
        let padding = 8 - used;
        write(&mut out, &mut byte, &mut used, (1 << padding) - 1, padding);
    }
    out.extend([0xff, 0xd9]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arw;
    use crate::tiff::Tiff;

    #[test]
    fn lossless_bayer_round_trip_all_predictors_and_partial_edge_tiles() {
        for (width, height, tw, th) in [(64, 48, 32, 16), (66, 50, 32, 32), (16, 16, 16, 16)] {
            let data = (0..width * height).map(|i| ((i * 7919 + i / width * 137) % 16384) as u16).collect();
            let file = Synthetic::new(width, height, data);
            for predictor in 1..=7 {
                let raw = arw::decode(&file.to_lossless_bytes(tw, th, predictor)).unwrap();
                assert_eq!(raw.data, file.data, "predictor {predictor}");
                assert_eq!(raw.black, [512.0; 4]);
                assert_eq!(raw.white, 16383.0);
                assert_eq!(raw.wb, [2600.0 / 1024.0, 1.0, 1600.0 / 1024.0]);
                assert_eq!((raw.crop.x, raw.crop.y, raw.crop.width, raw.crop.height), (0, 0, width, height));
                assert_eq!(serde_json::to_value(raw.compression).unwrap(), "lossless-compressed");
            }
        }
    }

    #[test]
    fn lossless_bad_tiff_arrays_and_dimensions_fail_cleanly() {
        let original = Synthetic::new(64, 48, vec![512; 64 * 48]).to_lossless_bytes(32, 16, 1);
        let (tiff, first) = Tiff::new(&original).unwrap();
        let ifd0 = tiff.ifd(first).unwrap();
        let raw_at = tiff.int(&ifd0, 0x014a).unwrap() as usize;
        let raw = tiff.ifd(raw_at as u32).unwrap();
        let patch = |tag: u16, value: u32| {
            let mut bytes = original.clone();
            let at = raw.entries[&tag].offset;
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            bytes
        };
        for bytes in [
            patch(0x0144, u32::MAX),
            patch(0x0145, u32::MAX),
            patch(0x0142, 0),
            patch(0x0143, 513),
            patch(0x0100, 65535),
            patch(0x0101, 65535),
        ] {
            let error = arw::decode(&bytes).err().expect("malformed tile accepted");
            assert!(matches!(error, crate::error::Error::Format(_) | crate::error::Error::Unsupported(_)));
        }
        // Aliased ranges must not amplify the work done on one JPEG payload.
        let mut aliased = original.clone();
        let at = raw.entries[&0x0144].offset;
        let first_offset = aliased[at..at + 4].to_vec();
        aliased[at + 4..at + 8].copy_from_slice(&first_offset);
        assert!(arw::decode(&aliased).is_err());
        // A tiny advertised tile must not create millions of table allocations.
        let mut excessive = patch(0x0142, 2);
        let at = raw.entries[&0x0100].offset;
        excessive[at..at + 2].copy_from_slice(&8192u16.to_le_bytes());
        assert!(arw::decode(&excessive).is_err());
        // Counts must be exact, never silently truncated by TIFF's limited reads.
        for tag in [0x0144, 0x0145] {
            let mut bytes = original.clone();
            let index = (0..raw.entries.len())
                .find(|&i| {
                    u16::from_le_bytes(bytes[raw_at + 2 + i * 12..raw_at + 4 + i * 12].try_into().unwrap()) == tag
                })
                .unwrap();
            let at = raw_at + 6 + index * 12;
            bytes[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
            assert!(arw::decode(&bytes).is_err());
        }
        for len in 0..original.len() {
            assert!(arw::decode(&original[..len]).is_err(), "accepted prefix {len}");
        }
        // Single-channel lossless output must never accept a YCbCr frame.
        assert!(matches!(arw::decode(&patch(0x0106, 6)), Err(crate::error::Error::Unsupported(_))));
    }

    #[test]
    fn preview_colour_tags_follow_the_raw_exif_chain() {
        let mut file = Synthetic::new(64, 48, vec![600; 64 * 48]);
        file.exif_color_space = Some(65535);
        file.interoperability_index = Some(*b"R03\0");
        file.embedded_jpeg = Some(b"bare preview bytes".to_vec());
        let bytes = file.to_bytes();
        let (tiff, first) = Tiff::new(&bytes).unwrap();
        let ifd0 = tiff.ifd(first).unwrap();
        assert!(tiff.int(&ifd0, 0xa001).is_none());
        let sensor = tiff.ifd(tiff.int(&ifd0, 0x014a).unwrap() as u32).unwrap();
        assert!(tiff.int(&sensor, 0xa001).is_none());
        let exif = tiff.ifd(tiff.int(&ifd0, 0x8769).unwrap() as u32).unwrap();
        assert_eq!(tiff.int(&exif, 0xa001), Some(65535));
        let interop = tiff.ifd(tiff.int(&exif, 0xa005).unwrap() as u32).unwrap();
        assert_eq!(tiff.string(&interop, 0x0001).as_deref(), Some("R03"));
        let raw = arw::decode(&bytes).unwrap();
        assert_eq!(raw.data, file.data);
        let (at, len) = raw.embedded_jpeg.unwrap();
        assert_eq!(&bytes[at..at + len], file.embedded_jpeg.as_ref().unwrap());
    }

    #[test]
    fn synthetic_file_decodes_to_the_same_samples() {
        let data: Vec<u16> = (0..64 * 48).map(|i| 512 + (i * 5 % 15000) as u16).collect();
        let mut file = Synthetic::new(64, 48, data.clone());
        file.orientation = 8;
        let raw = arw::decode(&file.to_bytes()).unwrap();
        assert_eq!((raw.width, raw.height, raw.cfa, raw.white), (64, 48, [0, 1, 1, 2], 16383.0));
        assert_eq!(raw.data, data);
        assert_eq!(raw.black, [512.0; 4]);
        assert_eq!(raw.meta.orientation, 8);
        assert_eq!(raw.meta.model.as_deref(), Some("ILCE-7M4"));
        assert!((raw.wb[0] - 2600.0 / 1024.0).abs() < 1e-6 && (raw.wb[2] - 1600.0 / 1024.0).abs() < 1e-6);
        assert_eq!((raw.crop.width, raw.crop.height), (64, 48));
    }

    #[test]
    fn damaged_files_are_rejected_not_misread() {
        let bytes = Synthetic::new(64, 48, vec![600; 64 * 48]).to_bytes();
        // Truncated pixel data.
        assert!(arw::decode(&bytes[..bytes.len() - 100]).is_err());
        // Not a TIFF at all.
        assert!(arw::decode(b"\xff\xd8\xff\xe0 definitely a jpeg").is_err());
        assert!(arw::decode(&[]).is_err());
        // Every prefix must fail cleanly rather than panic.
        for len in (0..bytes.len().min(400)).step_by(7) {
            let _ = arw::decode(&bytes[..len]);
        }
        // Sizes too small for the Bayer filters are refused, not mis-indexed.
        for (w, h) in [(1, 1), (2, 2), (9, 9), (14, 40)] {
            assert!(arw::decode(&Synthetic::new(w, h, vec![600; w * h]).to_bytes()).is_err(), "{w}x{h}");
        }
        // Flipping header bytes must never panic either.
        for i in 0..300 {
            for flip in [0xff, 0x80, 0x01] {
                let mut broken = bytes.clone();
                broken[i] ^= flip;
                // Whatever still decodes must also survive the first pipeline stages.
                if let Ok(raw) = arw::decode(&broken) {
                    crate::stats::raw_stats(&raw);
                    let mosaic = crate::demosaic::prepare(&raw, [2.0, 1.0, 1.5]);
                    crate::demosaic::half(&mosaic);
                    crate::demosaic::full(&mosaic);
                }
            }
        }
    }
}

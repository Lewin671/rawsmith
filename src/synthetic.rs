//! Builds small, valid uncompressed ARW files from pixel data. Tests and the
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

    pub fn to_bytes(&self) -> Vec<u8> {
        // Layout: header, IFD0, strings, raw IFD, its arrays, pixel data.
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
        let ifd0_entries = 4u32;
        let ifd0_end = 8 + 2 + ifd0_entries * 12 + 4;
        let make_at = ifd0_end;
        let model_at = make_at + 5;
        let raw_ifd_at = (model_at + model.len() as u32 + 1) & !1;
        let raw_entries = 13u32;
        let arrays_at = raw_ifd_at + 2 + raw_entries * 12 + 4;
        let (black_at, wb_at, origin_at, size_at) = (arrays_at, arrays_at + 8, arrays_at + 16, arrays_at + 24);
        let strip_at = arrays_at + 32;

        out.extend((ifd0_entries as u16).to_le_bytes());
        out.extend(entry(0x010f, 2, 5, make_at));
        out.extend(entry(0x0110, 2, model.len() as u32, model_at));
        out.extend(entry(0x0112, 3, 1, self.orientation as u32));
        out.extend(entry(0x014a, 4, 1, raw_ifd_at));
        out.extend(0u32.to_le_bytes());
        out.extend(b"SONY\0");
        out.extend(model.as_bytes());
        out.resize(raw_ifd_at as usize, 0);

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
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arw;

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

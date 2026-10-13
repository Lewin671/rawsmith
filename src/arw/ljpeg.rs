//! Lossless Huffman JPEG (T.81, Annexes B, C and H). Independently written
//! from the standard; restricted to Sony's four equally sampled CFA planes.

use crate::error::{Error, Result};

fn damaged(message: &str) -> Error {
    Error::Format(format!("lossless JPEG: {message}"))
}

#[derive(Default)]
struct Huffman {
    first: [u32; 16],
    count: [u32; 16],
    start: [usize; 16],
    symbols: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8], symbols: &[u8]) -> Result<Self> {
        let mut table = Self::default();
        let (mut code, mut start) = (0u32, 0usize);
        let mut seen = [false; 17];
        for i in 0..16 {
            let count = counts[i] as u32;
            // An all-ones code is forbidden (it would consume padding).
            if code + count >= 1 << (i + 1) {
                return Err(damaged("oversubscribed or all-ones Huffman table"));
            }
            table.first[i] = code;
            table.count[i] = count;
            table.start[i] = start;
            start += count as usize;
            code = (code + count) << 1;
        }
        if start == 0 || start != symbols.len() {
            return Err(damaged("empty or incomplete Huffman table"));
        }
        for &symbol in symbols {
            if symbol > 16 || seen[symbol as usize] {
                return Err(damaged("invalid or duplicate difference category"));
            }
            seen[symbol as usize] = true;
        }
        table.symbols = symbols.to_vec();
        Ok(table)
    }

    fn difference(&self, bits: &mut Bits<'_>) -> Result<i32> {
        let mut code = 0u32;
        for i in 0..16 {
            code = (code << 1) | bits.read(1)?;
            if code >= self.first[i] && code - self.first[i] < self.count[i] {
                let category = self.symbols[self.start[i] + (code - self.first[i]) as usize];
                return Ok(match category {
                    0 => 0,
                    // T.81 H.1.2.2: category 16 has no appended bits.
                    16 => 32768,
                    n => {
                        let value = bits.read(n)? as i32;
                        if value < 1 << (n - 1) {
                            value + 1 - (1 << n)
                        } else {
                            value
                        }
                    }
                });
            }
        }
        Err(damaged("invalid Huffman code"))
    }
}

struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
    byte: u8,
    remaining: u8,
}

impl Bits<'_> {
    fn read(&mut self, count: u8) -> Result<u32> {
        let mut value = 0;
        for _ in 0..count {
            if self.remaining == 0 {
                self.byte = *self.bytes.get(self.at).ok_or_else(|| damaged("truncated entropy data"))?;
                self.at += 1;
                if self.byte == 0xff {
                    if self.bytes.get(self.at) != Some(&0) {
                        return Err(damaged("unexpected marker in entropy data"));
                    }
                    self.at += 1;
                }
                self.remaining = 8;
            }
            self.remaining -= 1;
            value = (value << 1) | u32::from((self.byte >> self.remaining) & 1);
        }
        Ok(value)
    }

    fn finish(&self) -> Result<()> {
        let mask = (1u16 << self.remaining) - 1;
        if u16::from(self.byte) & mask != mask || self.bytes.get(self.at..self.at + 2) != Some(&[0xff, 0xd9]) {
            return Err(damaged("missing EOI or invalid entropy padding"));
        }
        // Sony aligns TIFF tile payloads to 16 bytes with 0xff fill.
        let tail = &self.bytes[self.at + 2..];
        if tail.len() > 15 || tail.iter().any(|&b| b != 0xff) {
            return Err(damaged("unexpected data after EOI"));
        }
        Ok(())
    }
}

pub(super) struct Tile<'a> {
    bytes: &'a [u8],
    entropy: usize,
    width: usize,
    height: usize,
    precision: u8,
    transform: u8,
    predictor: u8,
    tables: [Option<Huffman>; 4],
    selectors: [usize; 4],
}

impl<'a> Tile<'a> {
    pub(super) fn parse(bytes: &'a [u8], width: usize, height: usize, precision: u8) -> Result<Self> {
        if width == 0 || height == 0 || width > 256 || height > 256 || !(2..=16).contains(&precision) {
            return Err(damaged("invalid tile geometry or precision"));
        }
        if bytes.get(..2) != Some(&[0xff, 0xd8]) {
            return Err(damaged("missing SOI"));
        }
        let mut tile = Self {
            bytes,
            entropy: 0,
            width,
            height,
            precision,
            transform: 0,
            predictor: 0,
            tables: std::array::from_fn(|_| None),
            selectors: [0; 4],
        };
        let mut at = 2;
        let mut frame = false;
        loop {
            if bytes.get(at) != Some(&0xff) {
                return Err(damaged("missing marker"));
            }
            let marker = *bytes.get(at + 1).ok_or_else(|| damaged("truncated marker"))?;
            let length = bytes.get(at + 2..at + 4).ok_or_else(|| damaged("truncated segment"))?;
            let length = u16::from_be_bytes([length[0], length[1]]) as usize;
            if length < 2 {
                return Err(damaged("invalid segment length"));
            }
            let segment = bytes.get(at + 4..at + 2 + length).ok_or_else(|| damaged("truncated segment"))?;
            at += length + 2;
            if at > 4096 {
                return Err(damaged("implausibly large tile header"));
            }
            match marker {
                0xc3 => {
                    if frame || segment.len() != 18 {
                        return Err(damaged("invalid CFA frame"));
                    }
                    let h = u16::from_be_bytes([segment[1], segment[2]]) as usize;
                    let w = u16::from_be_bytes([segment[3], segment[4]]) as usize;
                    if segment[0] != precision || w != width || h != height || segment[5] != 4 {
                        return Err(damaged("frame does not match TIFF tile geometry/precision"));
                    }
                    for c in 0..4 {
                        if segment[6 + c * 3..9 + c * 3] != [c as u8 + 1, 0x11, 0] {
                            return Err(Error::Unsupported(
                                "lossless JPEG CFA component layout is not supported".into(),
                            ));
                        }
                    }
                    frame = true;
                }
                0xc4 => {
                    let mut rest = segment;
                    while !rest.is_empty() {
                        if rest.len() < 17 || rest[0] > 3 {
                            return Err(damaged("invalid DC Huffman table header"));
                        }
                        let id = rest[0] as usize;
                        if tile.tables[id].is_some() {
                            return Err(damaged("duplicate Huffman table definition"));
                        }
                        let count: usize = rest[1..17].iter().map(|&v| v as usize).sum();
                        let symbols = rest.get(17..17 + count).ok_or_else(|| damaged("truncated Huffman table"))?;
                        tile.tables[id] = Some(Huffman::new(&rest[1..17], symbols)?);
                        rest = &rest[17 + count..];
                    }
                }
                0xda => {
                    if !frame || segment.len() != 12 || segment[0] != 4 {
                        return Err(damaged("invalid CFA scan"));
                    }
                    for c in 0..4 {
                        let id = segment[2 + c * 2];
                        if segment[1 + c * 2] != c as u8 + 1 || id & 15 != 0 || id >> 4 > 3 {
                            return Err(damaged("invalid scan component/table selector"));
                        }
                        tile.selectors[c] = (id >> 4) as usize;
                        if tile.tables[tile.selectors[c]].is_none() {
                            return Err(damaged("scan references a missing Huffman table"));
                        }
                    }
                    tile.predictor = segment[9];
                    tile.transform = segment[11];
                    if !(1..=7).contains(&tile.predictor) || segment[10] != 0 || tile.transform >= precision {
                        return Err(damaged("invalid predictor or point transform"));
                    }
                    // Each difference consumes at least one bit. Check before allocation.
                    if bytes.len() - at < (width * height * 4).div_ceil(8) + 2 {
                        return Err(damaged("entropy data too short for tile dimensions"));
                    }
                    tile.entropy = at;
                    return Ok(tile);
                }
                0xe0..=0xef | 0xfe => {}
                _ => return Err(Error::Unsupported(format!("lossless JPEG marker 0xff{marker:02x} is not supported"))),
            }
        }
    }

    pub(super) fn decode(&self) -> Result<Vec<u16>> {
        let mut bits = Bits { bytes: &self.bytes[self.entropy..], at: 0, byte: 0, remaining: 0 };
        let mut out = vec![0u16; self.width * self.height * 4];
        for y in 0..self.height {
            for x in 0..self.width {
                for c in 0..4 {
                    let at = (y * self.width + x) * 4 + c;
                    let predict = if x == 0 && y == 0 {
                        1 << (self.precision - self.transform - 1)
                    } else if y == 0 {
                        i32::from(out[at - 4])
                    } else if x == 0 {
                        i32::from(out[at - self.width * 4])
                    } else {
                        let a = i32::from(out[at - 4]);
                        let b = i32::from(out[at - self.width * 4]);
                        let c = i32::from(out[at - self.width * 4 - 4]);
                        match self.predictor {
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
                    let difference = self.tables[self.selectors[c]].as_ref().unwrap().difference(&mut bits)?;
                    let value = (predict + difference) as u16;
                    if u32::from(value) >= 1 << (self.precision - self.transform) {
                        return Err(damaged("sample exceeds frame precision"));
                    }
                    out[at] = value;
                }
            }
        }
        bits.finish()?;
        for value in &mut out {
            *value <<= self.transform;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::lossless_jpeg;

    #[test]
    fn full_precision_modulo_category_16_and_point_transform() {
        for precision in [14, 16] {
            for transform in [0, 2] {
                for predictor in 1..=7 {
                    let data: Vec<u16> = (0..8 * 6 * 4)
                        .map(|i| {
                            let v = [0, 65532, 32768, 4, 60000, 16380, 8][i % 7];
                            v & if precision == 14 { 16383 } else { 65535 }
                        })
                        .collect();
                    let bytes = lossless_jpeg(&data, 8, 6, precision, predictor, transform);
                    let decoded = Tile::parse(&bytes, 8, 6, precision).unwrap().decode().unwrap();
                    assert_eq!(decoded, data);
                }
            }
        }
    }

    #[test]
    fn four_dc_tables_and_bounded_headers() {
        let data: Vec<u16> = (0..8 * 6 * 4).map(|i| (i * 1177 % 16384) as u16).collect();
        let original = lossless_jpeg(&data, 8, 6, 14, 1, 0);
        let dht = original.windows(2).position(|b| b == [0xff, 0xc4]).unwrap();
        let sos = original.windows(2).position(|b| b == [0xff, 0xda]).unwrap();
        let definition = &original[dht + 4..sos];
        let mut bytes = original[..dht + 2].to_vec();
        bytes.extend((2u16 + 4 * definition.len() as u16).to_be_bytes());
        for id in 0..4 {
            bytes.push(id);
            bytes.extend(&definition[1..]);
        }
        let scan_at = bytes.len();
        bytes.extend(&original[sos..]);
        for c in 0..4 {
            bytes[scan_at + 6 + c * 2] = (c as u8) << 4;
        }
        assert_eq!(Tile::parse(&bytes, 8, 6, 14).unwrap().decode().unwrap(), data);
        // Repeated definitions are rejected rather than repeatedly allocating.
        bytes[dht + 4 + definition.len()] = 0;
        assert!(Tile::parse(&bytes, 8, 6, 14).is_err());
        let mut large = original[..2].to_vec();
        large.extend([0xff, 0xfe]);
        large.extend(4096u16.to_be_bytes());
        large.extend(vec![0; 4094]);
        large.extend(&original[2..]);
        assert!(Tile::parse(&large, 8, 6, 14).is_err());
    }

    #[test]
    fn truncated_segments_and_entropy_and_missing_eoi_are_errors() {
        let data: Vec<u16> = (0..16 * 16 * 4).map(|i| (i * 19 % 16384) as u16).collect();
        let bytes = lossless_jpeg(&data, 16, 16, 14, 1, 0);
        for end in 0..bytes.len() {
            assert!(Tile::parse(&bytes[..end], 16, 16, 14).and_then(|t| t.decode()).is_err());
        }
        let mut aligned = bytes.clone();
        aligned.extend([0xff; 15]);
        assert_eq!(Tile::parse(&aligned, 16, 16, 14).unwrap().decode().unwrap(), data);
        aligned.push(0xff);
        assert!(Tile::parse(&aligned, 16, 16, 14).unwrap().decode().is_err());
        let mut broken = bytes.clone();
        *broken.last_mut().unwrap() = 0;
        assert!(Tile::parse(&broken, 16, 16, 14).unwrap().decode().is_err());
    }

    #[test]
    fn invalid_huffman_tables_and_scan_fields_are_errors() {
        let mut counts = [0u8; 16];
        assert!(Huffman::new(&counts, &[]).is_err());
        counts[0] = 2;
        assert!(Huffman::new(&counts, &[0, 1]).is_err());
        counts[0] = 3;
        assert!(Huffman::new(&counts, &[0, 1, 2]).is_err());
        counts[0] = 1;
        assert!(Huffman::new(&counts, &[17]).is_err());
        assert!(Huffman::new(&counts, &[]).is_err());
        counts[0] = 0;
        counts[1] = 2;
        assert!(Huffman::new(&counts, &[0, 0]).is_err());
        let bytes = lossless_jpeg(&vec![512; 16 * 16 * 4], 16, 16, 14, 1, 0);
        let dht = bytes.windows(2).position(|b| b == [0xff, 0xc4]).unwrap();
        let sos = bytes.windows(2).position(|b| b == [0xff, 0xda]).unwrap();
        for (at, value) in [
            (dht + 4, 0x10),
            (dht + 5, 255),
            (dht + 21, 17),
            (sos + 6, 0x30),
            (sos + 13, 0),
            (sos + 14, 1),
            (sos + 15, 14),
            (5, 0),
            (7, 0xff),
            (12, 0x22),
        ] {
            let mut broken = bytes.clone();
            broken[at] = value;
            assert!(Tile::parse(&broken, 16, 16, 14).and_then(|t| t.decode()).is_err(), "byte {at}");
        }
        let mut broken = bytes.clone();
        let entropy = Tile::parse(&broken, 16, 16, 14).unwrap().entropy;
        broken[entropy] = 0xf8; // Code 11111 is absent from the fixed table.
        assert!(Tile::parse(&broken, 16, 16, 14).unwrap().decode().is_err());
        broken[entropy..entropy + 2].copy_from_slice(&[0xff, 0xd0]);
        assert!(Tile::parse(&broken, 16, 16, 14).unwrap().decode().is_err());
    }
}

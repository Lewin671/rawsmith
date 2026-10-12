//! Minimal TIFF/EXIF container reader: just enough to walk IFDs and read tag
//! values. Every offset and count is bounds-checked because the input is an
//! untrusted file.

use std::collections::BTreeMap;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub kind: u16,
    pub count: u32,
    /// Absolute file offset of the value bytes (inline values included).
    pub offset: usize,
}

#[derive(Debug, Default)]
pub struct Ifd {
    pub entries: BTreeMap<u16, Entry>,
    pub next: u32,
}

pub struct Tiff<'a> {
    pub data: &'a [u8],
    big_endian: bool,
}

fn type_size(kind: u16) -> Option<usize> {
    Some(match kind {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

impl<'a> Tiff<'a> {
    /// Parses the header and returns the reader plus the first IFD offset.
    pub fn new(data: &'a [u8]) -> Result<(Self, u32)> {
        let big_endian = match data.get(..4) {
            Some([b'I', b'I', 42, 0]) => false,
            Some([b'M', b'M', 0, 42]) => true,
            _ => return Err(Error::Format("not a TIFF container".into())),
        };
        let tiff = Tiff { data, big_endian };
        let first = tiff.u32_at(4)?;
        Ok((tiff, first))
    }

    fn bytes(&self, offset: usize, len: usize) -> Result<&'a [u8]> {
        offset
            .checked_add(len)
            .and_then(|end| self.data.get(offset..end))
            .ok_or_else(|| Error::Format(format!("read of {len} bytes at {offset} is out of bounds")))
    }

    pub fn u16_at(&self, offset: usize) -> Result<u16> {
        let b = self.bytes(offset, 2)?;
        let b = [b[0], b[1]];
        Ok(if self.big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    }

    pub fn u32_at(&self, offset: usize) -> Result<u32> {
        let b = self.bytes(offset, 4)?;
        let b = [b[0], b[1], b[2], b[3]];
        Ok(if self.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }

    pub fn ifd(&self, offset: u32) -> Result<Ifd> {
        let offset = offset as usize;
        let count = self.u16_at(offset)? as usize;
        let mut ifd = Ifd::default();
        for i in 0..count {
            let at = offset + 2 + i * 12;
            let tag = self.u16_at(at)?;
            let kind = self.u16_at(at + 2)?;
            let count = self.u32_at(at + 4)?;
            let Some(size) = type_size(kind) else { continue };
            let total =
                size.checked_mul(count as usize).ok_or_else(|| Error::Format("tag value size overflows".into()))?;
            let value_at = if total <= 4 { at + 8 } else { self.u32_at(at + 8)? as usize };
            // A tag whose value lies outside the file is dropped rather than
            // failing the whole parse; required tags are checked by callers.
            if self.bytes(value_at, total).is_ok() {
                ifd.entries.insert(tag, Entry { kind, count, offset: value_at });
            }
        }
        ifd.next = self.u32_at(offset + 2 + count * 12).unwrap_or(0);
        Ok(ifd)
    }

    /// Reads up to `limit` integer values of any integral TIFF type.
    pub fn ints(&self, ifd: &Ifd, tag: u16, limit: usize) -> Option<Vec<i64>> {
        let e = ifd.entries.get(&tag)?;
        let n = (e.count as usize).min(limit);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(match e.kind {
                1 | 7 => self.data[e.offset + i] as i64,
                6 => self.data[e.offset + i] as i8 as i64,
                3 => self.u16_at(e.offset + i * 2).ok()? as i64,
                8 => self.u16_at(e.offset + i * 2).ok()? as i16 as i64,
                4 | 13 => self.u32_at(e.offset + i * 4).ok()? as i64,
                9 => self.u32_at(e.offset + i * 4).ok()? as i32 as i64,
                _ => return None,
            });
        }
        Some(out)
    }

    pub fn int(&self, ifd: &Ifd, tag: u16) -> Option<i64> {
        self.ints(ifd, tag, 1)?.first().copied()
    }

    /// Reads rational (or integral) values as floats.
    pub fn floats(&self, ifd: &Ifd, tag: u16, limit: usize) -> Option<Vec<f64>> {
        let e = ifd.entries.get(&tag)?;
        if e.kind != 5 && e.kind != 10 {
            return Some(self.ints(ifd, tag, limit)?.into_iter().map(|v| v as f64).collect());
        }
        let n = (e.count as usize).min(limit);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let num = self.u32_at(e.offset + i * 8).ok()?;
            let den = self.u32_at(e.offset + i * 8 + 4).ok()?;
            let (num, den) =
                if e.kind == 10 { (num as i32 as f64, den as i32 as f64) } else { (num as f64, den as f64) };
            out.push(if den == 0.0 { 0.0 } else { num / den });
        }
        Some(out)
    }

    pub fn float(&self, ifd: &Ifd, tag: u16) -> Option<f64> {
        self.floats(ifd, tag, 1)?.first().copied()
    }

    pub fn string(&self, ifd: &Ifd, tag: u16) -> Option<String> {
        let e = ifd.entries.get(&tag)?;
        if e.kind != 2 {
            return None;
        }
        let raw = &self.data[e.offset..e.offset + e.count as usize];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let text = String::from_utf8_lossy(&raw[..end]).trim().to_string();
        (!text.is_empty()).then_some(text)
    }
}

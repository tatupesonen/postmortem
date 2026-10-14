//! Bounds-checked decoding of fixed-layout binary structures.
//!
//! Evidence is hostile: every field read is checked against the buffer and
//! reports *what* was being read when it fails, so corrupt structures surface
//! as errors instead of panics.

use thiserror::Error;

/// A failed structured read from a byte buffer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("truncated data: need {needed} bytes at offset {offset}, buffer has {available}")]
pub struct Truncated {
    pub offset: usize,
    pub needed: usize,
    pub available: usize,
}

/// Bounds-checked little/big-endian field access on byte slices.
///
/// Implemented for `[u8]`, so any `&[u8]`/`Vec<u8>` gains the methods.
pub trait Bytes {
    /// Returns `len` bytes at `offset`.
    fn sub(&self, offset: usize, len: usize) -> Result<&[u8], Truncated>;

    /// Returns a fixed-size array at `offset`.
    fn array<const N: usize>(&self, offset: usize) -> Result<[u8; N], Truncated> {
        let s = self.sub(offset, N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(s);
        Ok(out)
    }

    fn u8_at(&self, offset: usize) -> Result<u8, Truncated> {
        Ok(self.array::<1>(offset)?[0])
    }
    fn u16_le(&self, offset: usize) -> Result<u16, Truncated> {
        self.array(offset).map(u16::from_le_bytes)
    }
    fn u16_be(&self, offset: usize) -> Result<u16, Truncated> {
        self.array(offset).map(u16::from_be_bytes)
    }
    fn u32_le(&self, offset: usize) -> Result<u32, Truncated> {
        self.array(offset).map(u32::from_le_bytes)
    }
    fn u32_be(&self, offset: usize) -> Result<u32, Truncated> {
        self.array(offset).map(u32::from_be_bytes)
    }
    fn u64_le(&self, offset: usize) -> Result<u64, Truncated> {
        self.array(offset).map(u64::from_le_bytes)
    }
    fn u64_be(&self, offset: usize) -> Result<u64, Truncated> {
        self.array(offset).map(u64::from_be_bytes)
    }
    fn i32_le(&self, offset: usize) -> Result<i32, Truncated> {
        self.array(offset).map(i32::from_le_bytes)
    }
    fn i64_le(&self, offset: usize) -> Result<i64, Truncated> {
        self.array(offset).map(i64::from_le_bytes)
    }
    /// Little-endian unsigned integer of 1..=8 bytes (e.g. NTFS run lists,
    /// 48-bit MFT references).
    fn uint_le(&self, offset: usize, width: usize) -> Result<u64, Truncated> {
        let s = self.sub(offset, width.min(8))?;
        Ok(s.iter()
            .rev()
            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b)))
    }
    /// Little-endian two's-complement signed integer of 1..=8 bytes.
    fn int_le(&self, offset: usize, width: usize) -> Result<i64, Truncated> {
        let width = width.clamp(1, 8);
        let raw = self.uint_le(offset, width)?;
        let shift = 64 - 8 * width as u32;
        Ok(((raw << shift) as i64) >> shift)
    }
}

impl Bytes for [u8] {
    fn sub(&self, offset: usize, len: usize) -> Result<&[u8], Truncated> {
        offset
            .checked_add(len)
            .and_then(|end| self.get(offset..end))
            .ok_or(Truncated {
                offset,
                needed: len,
                available: self.len(),
            })
    }
}

/// Decodes UTF-16LE, replacing invalid sequences, stopping at the first NUL.
pub fn utf16le_lossy(bytes: &[u8]) -> String {
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .take_while(|&u| u != 0);
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Decodes UTF-16BE, replacing invalid sequences, stopping at the first NUL.
pub fn utf16be_lossy(bytes: &[u8]) -> String {
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_be_bytes(c))
        .take_while(|&u| u != 0);
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Decodes a NUL-terminated (or full-length) byte string as UTF-8, lossily.
pub fn cstr_lossy(bytes: &[u8]) -> String {
    let s = bytes.split(|&b| b == 0).next().unwrap_or_default();
    String::from_utf8_lossy(s).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reads_endian_fields() {
        let b = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(b.u16_le(0).unwrap(), 0x0201);
        assert_eq!(b.u16_be(0).unwrap(), 0x0102);
        assert_eq!(b.u32_le(4).unwrap(), 0x0807_0605);
        assert_eq!(b.u64_be(0).unwrap(), 0x0102_0304_0506_0708);
        assert_eq!(b.uint_le(0, 3).unwrap(), 0x03_0201);
    }

    #[test]
    fn truncation_reports_context() {
        let b = [0u8; 3];
        assert_eq!(
            b.u32_le(1),
            Err(Truncated {
                offset: 1,
                needed: 4,
                available: 3
            })
        );
        assert!(b.sub(usize::MAX, 2).is_err());
    }

    #[test]
    fn signed_variable_width() {
        assert_eq!([0xffu8].int_le(0, 1).unwrap(), -1);
        assert_eq!([0x00u8, 0x80].int_le(0, 2).unwrap(), -32768);
        assert_eq!([0x7fu8, 0x00].int_le(0, 2).unwrap(), 127);
        assert_eq!([0x10u8, 0x27, 0x00].int_le(0, 3).unwrap(), 10_000);
    }

    #[test]
    fn utf16_decoding() {
        let s: Vec<u8> = "héllo".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(utf16le_lossy(&s), "héllo");
        assert_eq!(utf16le_lossy(&[b'a', 0, 0, 0, b'b', 0]), "a");
        assert_eq!(utf16be_lossy(&[0, b'O', 0, b'K']), "OK");
        assert_eq!(utf16le_lossy(&[0x00, 0xd8, b'x', 0]), "\u{fffd}x");
    }

    #[test]
    fn cstr() {
        assert_eq!(cstr_lossy(b"abc\0def"), "abc");
        assert_eq!(cstr_lossy(b"abc"), "abc");
    }

    proptest! {
        #[test]
        fn never_panics(data in proptest::collection::vec(any::<u8>(), 0..32), off in any::<usize>(), w in 0usize..12) {
            let _ = data.uint_le(off, w);
            let _ = data.int_le(off, w);
            let _ = data.u64_le(off);
            let _ = utf16le_lossy(&data);
        }

        #[test]
        fn int_le_roundtrip(v in any::<i32>()) {
            prop_assert_eq!(v.to_le_bytes().int_le(0, 4).unwrap(), i64::from(v));
        }
    }
}

//! The 4 KiB base block (file header) of a hive or transaction log.

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;

use crate::{Error, Result};

/// Size of the base block; hive bins start right after it.
pub(crate) const BASE_BLOCK_SIZE: usize = 4096;
/// Bytes covered by the base-block checksum.
const CHECKSUM_LEN: usize = 508;

/// What a `regf` file is, from the base block's file-type field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiveFileType {
    /// A primary hive file.
    Primary,
    /// An old-format transaction log (`.LOG`, `.LOG1`/`.LOG2` before 8.1).
    LogOld,
    /// An old-format volatile log (Windows XP `.LOG` of some hives).
    LogVolatile,
    /// A new-format transaction log (Windows 8.1+).
    LogNew,
    /// Any other value.
    Other(u32),
}

impl HiveFileType {
    fn from_code(c: u32) -> Self {
        match c {
            0 => Self::Primary,
            1 => Self::LogOld,
            2 => Self::LogVolatile,
            6 => Self::LogNew,
            o => Self::Other(o),
        }
    }
}

/// Decoded base block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseBlock {
    /// Incremented before a write starts.
    pub primary_seq: u32,
    /// Set equal to `primary_seq` once a write completed.
    pub secondary_seq: u32,
    /// FILETIME of the last write.
    pub last_written: u64,
    pub major_version: u32,
    pub minor_version: u32,
    pub file_type: HiveFileType,
    /// Offset of the root key's cell (relative to the hive bins).
    pub root_offset: u32,
    /// Size of all hive bins in bytes.
    pub hbins_size: u32,
    /// Embedded file name (the tail of the hive's path), UTF-16.
    pub file_name: String,
    /// Stored checksum.
    pub checksum: u32,
    /// Whether the stored checksum matches the computed one.
    pub checksum_ok: bool,
}

impl BaseBlock {
    /// Parses the first 512 bytes of a base block.
    pub fn parse(b: &[u8]) -> Result<Self> {
        if b.get(..4) != Some(b"regf") {
            return Err(Error::NotRegf);
        }
        let checksum = b.u32_le(508)?;
        let computed = checksum_of(b.sub(0, CHECKSUM_LEN)?);
        Ok(Self {
            primary_seq: b.u32_le(4)?,
            secondary_seq: b.u32_le(8)?,
            last_written: b.u64_le(12)?,
            major_version: b.u32_le(20)?,
            minor_version: b.u32_le(24)?,
            file_type: HiveFileType::from_code(b.u32_le(28)?),
            root_offset: b.u32_le(36)?,
            hbins_size: b.u32_le(40)?,
            file_name: utf16le_lossy(b.sub(48, 64)?),
            checksum,
            checksum_ok: checksum == computed,
        })
    }

    /// A hive whose last write did not complete (sequence numbers differ)
    /// or whose header is damaged; transaction logs should be replayed.
    pub fn is_dirty(&self) -> bool {
        self.primary_seq != self.secondary_seq || !self.checksum_ok
    }
}

/// The base-block checksum: XOR of the first 127 dwords, with 0 and
/// `0xFFFF_FFFF` remapped as Windows does.
pub(crate) fn checksum_of(b: &[u8]) -> u32 {
    let x = b
        .as_chunks::<4>()
        .0
        .iter()
        .take(CHECKSUM_LEN / 4)
        .fold(0u32, |acc, c| acc ^ u32::from_le_bytes(*c));
    match x {
        0 => 1,
        0xFFFF_FFFF => 0xFFFF_FFFE,
        x => x,
    }
}

/// Rewrites the checksum of a base block in place.
pub(crate) fn fix_checksum(b: &mut [u8]) {
    if let Some(head) = b.get(..CHECKSUM_LEN) {
        let c = checksum_of(head);
        if let Some(dst) = b.get_mut(CHECKSUM_LEN..CHECKSUM_LEN + 4) {
            dst.copy_from_slice(&c.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_remaps_sentinels() {
        let zero = [0u8; 512];
        assert_eq!(checksum_of(&zero), 1);
        let mut ones = [0u8; 512];
        ones[..4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert_eq!(checksum_of(&ones), 0xFFFF_FFFE);
    }

    #[test]
    fn parse_rejects_non_regf() {
        assert!(matches!(BaseBlock::parse(&[0u8; 512]), Err(Error::NotRegf)));
        assert!(BaseBlock::parse(b"regf").is_err());
    }

    #[test]
    fn parse_and_fix() {
        let mut b = vec![0u8; 4096];
        b[..4].copy_from_slice(b"regf");
        b[4..8].copy_from_slice(&3u32.to_le_bytes());
        b[8..12].copy_from_slice(&3u32.to_le_bytes());
        b[36..40].copy_from_slice(&0x20u32.to_le_bytes());
        b[28..32].copy_from_slice(&6u32.to_le_bytes());
        fix_checksum(&mut b);
        let bb = BaseBlock::parse(&b).unwrap();
        assert!(bb.checksum_ok);
        assert!(!bb.is_dirty());
        assert_eq!(bb.root_offset, 0x20);
        assert_eq!(bb.file_type, HiveFileType::LogNew);
        b[4] = 4;
        let bb = BaseBlock::parse(&b).unwrap();
        assert!(bb.is_dirty());
    }
}

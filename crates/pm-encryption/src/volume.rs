//! Volume-level encryption signatures: Autopsy's BitLocker check
//! (`BitlockerDetection`) plus LUKS and Core Storage (FileVault 2), which
//! Autopsy does not recognise.

/// Bytes of a volume's start the checks need.
pub const VOLUME_HEAD_LEN: usize = 512;

/// A recognised volume encryption format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeEncryption {
    /// BitLocker (Autopsy's rule).
    BitLocker,
    /// Linux Unified Key Setup, version 1 or 2.
    Luks,
    /// An Apple Core Storage physical volume (FileVault 2 on HFS+ Macs;
    /// also used unencrypted by Fusion Drives).
    CoreStorage,
}

/// `BitlockerDetection.isBitlockerVolume` on the first 0x54 bytes:
/// `-FVE-FS-` at offset 3, a power-of-two sectors-per-cluster byte below
/// 0x80 (Autopsy compares the signed byte, so 0x80 never matches), and
/// zero reserved/FAT/root/sector-count fields.
pub fn is_bitlocker(head: &[u8]) -> bool {
    let mut bpb = [0u8; 0x54];
    for (d, s) in bpb.iter_mut().zip(head) {
        *d = *s;
    }
    if bpb.get(3..11) != Some(b"-FVE-FS-".as_slice()) {
        return false;
    }
    if !matches!(bpb[0x0D], 0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x20 | 0x40) {
        return false;
    }
    let zero = |r: std::ops::Range<usize>| bpb.get(r).is_some_and(|b| b.iter().all(|&x| x == 0));
    zero(0x0E..0x10)
        && bpb[0x10] == 0
        && zero(0x11..0x13)
        && zero(0x13..0x15)
        && zero(0x16..0x18)
        && zero(0x20..0x24)
}

/// LUKS1/LUKS2 header magic and version.
pub fn is_luks(head: &[u8]) -> bool {
    head.starts_with(b"LUKS\xBA\xBE") && matches!(head.get(6..8), Some([0, 1 | 2]))
}

/// Core Storage physical volume header: format version 1, block type
/// 0x0010 and the `CS` signature at offset 88.
pub fn is_core_storage(head: &[u8]) -> bool {
    head.get(8..12) == Some([1, 0, 0x10, 0].as_slice())
        && head.get(88..90) == Some(b"CS".as_slice())
}

/// The first format `head` (a volume's first bytes) matches, BitLocker
/// first as in Autopsy.
pub fn detect(head: &[u8]) -> Option<VolumeEncryption> {
    if is_bitlocker(head) {
        Some(VolumeEncryption::BitLocker)
    } else if is_luks(head) {
        Some(VolumeEncryption::Luks)
    } else if is_core_storage(head) {
        Some(VolumeEncryption::CoreStorage)
    } else {
        None
    }
}

/// Test fixtures: volume boot records of each kind.
#[cfg(any(test, feature = "builder"))]
#[allow(
    clippy::indexing_slicing,
    reason = "fixture builder with fixed layouts"
)]
pub mod builder {
    /// A BitLocker volume boot record (Windows 7+ layout).
    pub fn bitlocker_vbr() -> Vec<u8> {
        let mut v = vec![0u8; 512];
        v[..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
        v[3..11].copy_from_slice(b"-FVE-FS-");
        v[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        v[0x0D] = 8;
        v[0x15] = 0xF8;
        v[0x18..0x1A].copy_from_slice(&63u16.to_le_bytes());
        v[0x1A..0x1C].copy_from_slice(&255u16.to_le_bytes());
        v[0x1C..0x20].copy_from_slice(&2048u32.to_le_bytes());
        v[510] = 0x55;
        v[511] = 0xAA;
        v
    }

    /// A LUKS1 header start.
    pub fn luks_header() -> Vec<u8> {
        let mut v = vec![0u8; 592];
        v[..6].copy_from_slice(b"LUKS\xBA\xBE");
        v[6..8].copy_from_slice(&1u16.to_be_bytes());
        v[8..11].copy_from_slice(b"aes");
        v
    }

    /// A Core Storage physical volume header start.
    pub fn core_storage_header() -> Vec<u8> {
        let mut v = vec![0u8; 512];
        v[8..10].copy_from_slice(&1u16.to_le_bytes());
        v[10..12].copy_from_slice(&0x10u16.to_le_bytes());
        v[88..90].copy_from_slice(b"CS");
        v
    }
}

#[cfg(test)]
mod tests {
    use super::builder::*;
    use super::*;

    #[test]
    fn bitlocker_rules() {
        let v = bitlocker_vbr();
        assert!(is_bitlocker(&v));
        assert_eq!(detect(&v), Some(VolumeEncryption::BitLocker));
        // Autopsy's signed-byte comparison rejects 0x80 sectors/cluster.
        let mut w = v.clone();
        w[0x0D] = 0x80;
        assert!(!is_bitlocker(&w));
        for (off, val) in [
            (0x0D, 3u8),
            (0x0E, 1),
            (0x10, 2),
            (0x12, 1),
            (0x13, 1),
            (0x17, 1),
            (0x23, 1),
            (5, b'X'),
        ] {
            let mut w = v.clone();
            w[off] = val;
            assert!(!is_bitlocker(&w), "offset {off:#x}");
        }
        // An NTFS boot sector is not BitLocker.
        let mut ntfs = v;
        ntfs[3..11].copy_from_slice(b"NTFS    ");
        assert!(!is_bitlocker(&ntfs));
        assert!(!is_bitlocker(&[]));
    }

    #[test]
    fn luks_and_core_storage() {
        assert_eq!(detect(&luks_header()), Some(VolumeEncryption::Luks));
        let mut l = luks_header();
        l[7] = 3;
        assert_eq!(detect(&l), None);
        assert_eq!(
            detect(&core_storage_header()),
            Some(VolumeEncryption::CoreStorage)
        );
        assert_eq!(detect(&[0u8; 512]), None);
    }
}

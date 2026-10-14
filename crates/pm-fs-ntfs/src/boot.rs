//! NTFS boot sector.

use pm_core::Bytes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Boot {
    pub(crate) bytes_per_sector: u32,
    pub(crate) cluster_size: u64,
    pub(crate) total_sectors: u64,
    pub(crate) mft_lcn: u64,
    pub(crate) mftmirr_lcn: u64,
    pub(crate) mft_record_size: u32,
    pub(crate) index_record_size: u32,
    pub(crate) serial: u64,
}

impl Boot {
    pub(crate) fn cluster_count(&self) -> u64 {
        self.total_sectors
            .saturating_mul(u64::from(self.bytes_per_sector))
            / self.cluster_size
    }
}

/// Decodes the "clusters per record" encoding: positive = clusters,
/// negative = 2^-n bytes.
fn record_size(raw: i8, cluster_size: u64) -> Option<u32> {
    let bytes = if raw > 0 {
        u64::from(raw.unsigned_abs()) * cluster_size
    } else {
        1u64.checked_shl(u32::from(raw.unsigned_abs()))?
    };
    u32::try_from(bytes)
        .ok()
        .filter(|b| (256..=65_536).contains(b) && b.is_power_of_two())
}

pub(crate) fn parse(b: &[u8]) -> Result<Boot, String> {
    let e = |t: pm_core::Truncated| t.to_string();
    if b.sub(3, 8).map_err(e)? != b"NTFS    " {
        return Err("missing NTFS OEM identifier".into());
    }
    if b.u16_le(510).map_err(e)? != 0xAA55 {
        return Err("missing boot signature".into());
    }
    let bps = u32::from(b.u16_le(11).map_err(e)?);
    if !matches!(bps, 256 | 512 | 1024 | 2048 | 4096) {
        return Err(format!("invalid bytes per sector {bps}"));
    }
    let spc_raw = b.u8_at(13).map_err(e)?;
    // Values above 0x80 encode 2^(256 - n) (cluster sizes > 64 KiB).
    let spc = if spc_raw > 0x80 {
        1u64 << (256 - u32::from(spc_raw)).min(31)
    } else {
        u64::from(spc_raw)
    };
    if spc == 0 || !spc.is_power_of_two() {
        return Err(format!("invalid sectors per cluster {spc_raw:#x}"));
    }
    let cluster_size = spc * u64::from(bps);
    if cluster_size > 2 * 1024 * 1024 {
        return Err(format!("cluster size {cluster_size} too large"));
    }
    let total_sectors = b.u64_le(40).map_err(e)?;
    let mft_lcn = b.u64_le(48).map_err(e)?;
    let mftmirr_lcn = b.u64_le(56).map_err(e)?;
    let mft_record_size = record_size(b.u8_at(64).map_err(e)? as i8, cluster_size)
        .ok_or("invalid MFT record size")?;
    let index_record_size = record_size(b.u8_at(68).map_err(e)? as i8, cluster_size)
        .ok_or("invalid index record size")?;
    let boot = Boot {
        bytes_per_sector: bps,
        cluster_size,
        total_sectors,
        mft_lcn,
        mftmirr_lcn,
        mft_record_size,
        index_record_size,
        serial: b.u64_le(72).map_err(e)?,
    };
    if boot.mft_lcn >= boot.cluster_count().max(1) {
        return Err(format!("MFT cluster {mft_lcn} beyond volume"));
    }
    Ok(boot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_size_encoding() {
        assert_eq!(record_size(-10, 4096), Some(1024));
        assert_eq!(record_size(-12, 4096), Some(4096));
        assert_eq!(record_size(1, 4096), Some(4096));
        assert_eq!(record_size(2, 512), Some(1024));
        assert_eq!(record_size(0, 4096), None);
        assert_eq!(record_size(-128, 4096), None);
        assert_eq!(record_size(3, 512), None, "1536 is not a power of two");
    }
}

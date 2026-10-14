//! FAT boot sector (BPB) parsing and geometry.

use pm_core::Bytes;
use pm_fs::FsKind;

/// Parsed BIOS parameter block and derived geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub(crate) kind: FsKind,
    pub(crate) bytes_per_sector: u32,
    pub(crate) sectors_per_cluster: u32,
    pub(crate) reserved_sectors: u32,
    pub(crate) num_fats: u32,
    pub(crate) fat_sectors: u32,
    pub(crate) root_entries: u32,
    pub(crate) total_sectors: u64,
    /// First sector of the FAT12/16 fixed root directory.
    pub(crate) root_dir_sector: u64,
    pub(crate) root_dir_sectors: u64,
    pub(crate) first_data_sector: u64,
    pub(crate) cluster_count: u32,
    /// FAT32 root directory start cluster.
    pub(crate) root_cluster: u32,
    /// Index of the FAT to read (FAT32 may disable mirroring).
    pub(crate) active_fat: u32,
    pub(crate) volume_id: Option<u32>,
    pub(crate) label: Option<String>,
}

impl Geometry {
    pub(crate) fn cluster_size(&self) -> u64 {
        u64::from(self.bytes_per_sector) * u64::from(self.sectors_per_cluster)
    }

    /// Byte offset of a data cluster (`2..cluster_count + 2`).
    pub(crate) fn cluster_offset(&self, cluster: u32) -> u64 {
        (self.first_data_sector + u64::from(cluster - 2) * u64::from(self.sectors_per_cluster))
            * u64::from(self.bytes_per_sector)
    }

    pub(crate) fn is_valid_cluster(&self, cluster: u32) -> bool {
        cluster >= 2 && u64::from(cluster) < u64::from(self.cluster_count) + 2
    }

    /// Byte offset and length of FAT number `n`.
    pub(crate) fn fat_region(&self, n: u32) -> (u64, u64) {
        let bps = u64::from(self.bytes_per_sector);
        let start =
            (u64::from(self.reserved_sectors) + u64::from(n) * u64::from(self.fat_sectors)) * bps;
        (start, u64::from(self.fat_sectors) * bps)
    }

    pub(crate) fn fs_bytes(&self) -> u64 {
        self.total_sectors * u64::from(self.bytes_per_sector)
    }
}

/// Parses a boot sector; `Err` explains why the sector is not FAT.
pub(crate) fn parse(b: &[u8]) -> Result<Geometry, String> {
    let e = |t: pm_core::Truncated| t.to_string();
    if b.u16_le(510).map_err(e)? != 0xAA55 {
        return Err("missing 0x55AA boot signature".into());
    }
    let bps = u32::from(b.u16_le(11).map_err(e)?);
    if !matches!(bps, 512 | 1024 | 2048 | 4096) {
        return Err(format!("invalid bytes per sector {bps}"));
    }
    let spc = u32::from(b.u8_at(13).map_err(e)?);
    if spc == 0 || !spc.is_power_of_two() {
        return Err(format!("invalid sectors per cluster {spc}"));
    }
    let reserved = u32::from(b.u16_le(14).map_err(e)?);
    if reserved == 0 {
        return Err("zero reserved sectors".into());
    }
    let num_fats = u32::from(b.u8_at(16).map_err(e)?);
    if !(1..=4).contains(&num_fats) {
        return Err(format!("invalid FAT count {num_fats}"));
    }
    let root_entries = u32::from(b.u16_le(17).map_err(e)?);
    let total16 = u32::from(b.u16_le(19).map_err(e)?);
    let fat16_size = u32::from(b.u16_le(22).map_err(e)?);
    let total32 = b.u32_le(32).map_err(e)?;
    let fat32_size = b.u32_le(36).map_err(e)?;
    let fat_sectors = if fat16_size != 0 {
        fat16_size
    } else {
        fat32_size
    };
    if fat_sectors == 0 {
        return Err("zero FAT size".into());
    }
    let total_sectors = u64::from(if total16 != 0 { total16 } else { total32 });
    let root_dir_sectors = (u64::from(root_entries) * 32).div_ceil(u64::from(bps));
    let root_dir_sector = u64::from(reserved) + u64::from(num_fats) * u64::from(fat_sectors);
    let first_data_sector = root_dir_sector + root_dir_sectors;
    if total_sectors <= first_data_sector {
        return Err("no data area".into());
    }
    let clusters = (total_sectors - first_data_sector) / u64::from(spc);
    let cluster_count =
        u32::try_from(clusters).map_err(|_| "cluster count overflows".to_owned())?;

    // Microsoft's rule: the FAT type is determined by the cluster count alone.
    let kind = if cluster_count < 4085 {
        FsKind::Fat12
    } else if cluster_count < 65525 {
        FsKind::Fat16
    } else {
        FsKind::Fat32
    };
    if kind == FsKind::Fat32 && root_entries != 0 {
        return Err("FAT32 with fixed root directory entries".into());
    }
    if kind != FsKind::Fat32 && root_entries == 0 {
        return Err("FAT12/16 without root directory entries".into());
    }
    // The FAT must be large enough to describe every cluster.
    let needed_bits = (u64::from(cluster_count) + 2)
        * match kind {
            FsKind::Fat12 => 12,
            FsKind::Fat16 => 16,
            _ => 32,
        };
    if u64::from(fat_sectors) * u64::from(bps) * 8 < needed_bits {
        return Err("FAT too small for cluster count".into());
    }

    let (root_cluster, active_fat, id_off, label_off) = if kind == FsKind::Fat32 {
        let ext_flags = b.u16_le(40).map_err(e)?;
        let active = if ext_flags & 0x80 != 0 {
            u32::from(ext_flags & 0x0F)
        } else {
            0
        };
        (b.u32_le(44).map_err(e)?, active.min(num_fats - 1), 67, 71)
    } else {
        (0, 0, 39, 43)
    };
    let boot_sig_off = if kind == FsKind::Fat32 { 66 } else { 38 };
    let has_ext = matches!(b.u8_at(boot_sig_off), Ok(0x28 | 0x29));
    let volume_id = has_ext.then(|| b.u32_le(id_off).ok()).flatten();
    let label = has_ext
        .then(|| b.sub(label_off, 11).ok())
        .flatten()
        .map(|l| String::from_utf8_lossy(l).trim_end().to_owned())
        .filter(|l| !l.is_empty() && l != "NO NAME");

    Ok(Geometry {
        kind,
        bytes_per_sector: bps,
        sectors_per_cluster: spc,
        reserved_sectors: reserved,
        num_fats,
        fat_sectors,
        root_entries,
        total_sectors,
        root_dir_sector,
        root_dir_sectors,
        first_data_sector,
        cluster_count,
        root_cluster,
        active_fat,
        volume_id,
        label,
    })
}

//! Volume-system (partition table) parsing.
//!
//! [`detect`] probes a byte source for a GPT or DOS/MBR partition table and
//! returns a [`VolumeSystem`]. Like The Sleuth Kit, the result describes the
//! *whole* disk: allocated partitions plus `Meta` slots (partition tables,
//! extended containers) and `Unallocated` gaps, so investigators can reach
//! every byte of the evidence.

mod gpt;
mod layout;
mod mbr;
mod types;

use pm_core::ReadAt;
use pm_core::io::SliceSource;
use thiserror::Error;

pub use types::{gpt_type_name, mbr_type_name};

/// Errors raised while parsing a volume system.
#[derive(Debug, Error)]
pub enum VolumeError {
    #[error("I/O error reading volume system: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt {kind} volume system: {reason}")]
    Corrupt { kind: VsKind, reason: String },
}

/// Partitioning scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VsKind {
    /// DOS/MBR partition table, including extended partition chains.
    Dos,
    /// GUID Partition Table.
    Gpt,
}

impl std::fmt::Display for VsKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Dos => "DOS",
            Self::Gpt => "GPT",
        })
    }
}

/// What a slot in the volume layout represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotKind {
    /// A partition that holds data (and possibly a filesystem).
    Allocated,
    /// Space not covered by any partition or table.
    Unallocated,
    /// Partitioning metadata: tables, GPT headers, extended containers.
    Meta,
}

/// The raw partition type identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartitionType {
    Mbr(u8),
    Gpt(uuid::Uuid),
}

/// One slot of the volume layout, in sectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// Sequential address in layout order (stable for a given disk).
    pub addr: u32,
    pub kind: SlotKind,
    pub start_sector: u64,
    pub sector_count: u64,
    pub ptype: Option<PartitionType>,
    /// Human-readable description, e.g. `NTFS / exFAT (0x07)`.
    pub description: String,
    /// GPT partition name, if any.
    pub name: Option<String>,
    /// GPT unique partition GUID.
    pub unique_id: Option<uuid::Uuid>,
    /// Index of the partition table the entry came from (`0` = primary).
    pub table: Option<u32>,
    /// Index of the entry within its table.
    pub slot: Option<u32>,
    /// MBR boot flag / GPT attribute bits.
    pub flags: u64,
}

impl Partition {
    pub fn end_sector(&self) -> u64 {
        self.start_sector.saturating_add(self.sector_count)
    }
}

/// A parsed volume system covering the whole source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSystem {
    pub kind: VsKind,
    pub sector_size: u32,
    /// Size of the underlying source in sectors.
    pub disk_sectors: u64,
    /// GPT disk GUID.
    pub disk_guid: Option<uuid::Uuid>,
    /// All slots in ascending start order (ties: meta, allocated, unallocated).
    pub partitions: Vec<Partition>,
    /// Non-fatal problems (bad primary GPT, partitions past end of image, ...).
    pub warnings: Vec<String>,
}

impl VolumeSystem {
    /// Allocated (data) partitions only.
    pub fn allocated(&self) -> impl Iterator<Item = &Partition> {
        self.partitions
            .iter()
            .filter(|p| p.kind == SlotKind::Allocated)
    }

    /// Byte offset of a partition.
    pub fn byte_offset(&self, p: &Partition) -> u64 {
        p.start_sector.saturating_mul(u64::from(self.sector_size))
    }

    /// Byte length of a partition.
    pub fn byte_len(&self, p: &Partition) -> u64 {
        p.sector_count.saturating_mul(u64::from(self.sector_size))
    }

    /// Opens a partition's content as a byte source (clamped to the image).
    pub fn open<S: ReadAt>(&self, source: S, p: &Partition) -> SliceSource<S> {
        SliceSource::new(source, self.byte_offset(p), self.byte_len(p))
    }
}

/// Sector sizes probed for GPT, in order.
const GPT_SECTOR_SIZES: [u32; 2] = [512, 4096];

/// Detects and parses a volume system. Returns `Ok(None)` when the source
/// does not carry a recognised partition table (e.g. a bare filesystem).
pub fn detect(src: &dyn ReadAt) -> Result<Option<VolumeSystem>, VolumeError> {
    let Some(mbr) = mbr::read_table(src, 0)? else {
        // GPT disks always carry a protective MBR; without a valid boot
        // signature we may still find a GPT on disks with a damaged sector 0.
        return gpt_any(src);
    };
    if mbr.iter().any(|e| e.ptype == 0xEE) {
        if let Some(vs) = gpt_any(src)? {
            return Ok(Some(vs));
        }
        tracing::warn!("protective MBR present but no valid GPT; falling back to DOS");
    }
    mbr::parse(src, 512)
}

fn gpt_any(src: &dyn ReadAt) -> Result<Option<VolumeSystem>, VolumeError> {
    for ss in GPT_SECTOR_SIZES {
        if let Some(vs) = gpt::parse(src, ss)? {
            return Ok(Some(vs));
        }
    }
    Ok(None)
}

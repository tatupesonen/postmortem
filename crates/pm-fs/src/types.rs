//! Types shared by all filesystem implementations.

use bitflags::bitflags;
use jiff::Timestamp;

/// Metadata address (inode number, MFT entry, FAT directory-entry address…).
pub type InodeId = u64;

/// Filesystem type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FsKind {
    Ntfs,
    Fat12,
    Fat16,
    Fat32,
    ExFat,
    Ext2,
    Ext3,
    Ext4,
    HfsPlus,
    Apfs,
    Iso9660,
    Udf,
    Xfs,
    Btrfs,
    Yaffs2,
}

impl FsKind {
    /// Display name as used in the UI and reports.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ntfs => "NTFS",
            Self::Fat12 => "FAT12",
            Self::Fat16 => "FAT16",
            Self::Fat32 => "FAT32",
            Self::ExFat => "exFAT",
            Self::Ext2 => "Ext2",
            Self::Ext3 => "Ext3",
            Self::Ext4 => "Ext4",
            Self::HfsPlus => "HFS+",
            Self::Apfs => "APFS",
            Self::Iso9660 => "ISO9660",
            Self::Udf => "UDF",
            Self::Xfs => "XFS",
            Self::Btrfs => "Btrfs",
            Self::Yaffs2 => "YAFFS2",
        }
    }
}

impl std::fmt::Display for FsKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Filesystem geometry and identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsInfo {
    pub kind: FsKind,
    pub block_size: u32,
    pub block_count: u64,
    pub root_inode: InodeId,
    pub first_inode: InodeId,
    pub last_inode: InodeId,
    pub label: Option<String>,
    /// Volume serial / UUID, formatted as the filesystem conventionally does.
    pub serial: Option<String>,
}

/// File type as recorded in metadata (or, for names, in the directory entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    CharDevice,
    BlockDevice,
    Fifo,
    Socket,
    /// Synthesised by the parser (e.g. FAT `$MBR`, `$FAT1`).
    Virtual,
    #[default]
    Unknown,
}

bitflags! {
    /// Allocation and attribute state of a metadata entry.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct MetaFlags: u16 {
        /// Metadata entry is in use.
        const ALLOCATED = 1 << 0;
        /// Metadata entry has been used at some point (vs. never-used).
        const USED = 1 << 1;
        const COMPRESSED = 1 << 2;
        const ENCRYPTED = 1 << 3;
        const SPARSE = 1 << 4;
        /// Not reachable from the directory tree.
        const ORPHAN = 1 << 5;
        /// Recovery is unreliable (e.g. deleted FAT file, chain guessed).
        const RECOVERED_BEST_EFFORT = 1 << 6;
    }
}

/// The four classic timestamps. Absent values are `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Times {
    pub modified: Option<Timestamp>,
    pub accessed: Option<Timestamp>,
    /// Metadata change time (ctime / NTFS MFT-modified).
    pub changed: Option<Timestamp>,
    pub created: Option<Timestamp>,
}

/// Metadata of one inode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub inode: InodeId,
    pub kind: FileKind,
    pub flags: MetaFlags,
    /// Size of the default data stream in bytes.
    pub size: u64,
    pub times: Times,
    /// NTFS `$FILE_NAME` times (often more trustworthy against timestomping).
    pub fn_times: Option<Times>,
    /// POSIX mode bits including type bits, if the filesystem has them.
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub link_count: u32,
    /// NTFS sequence number / ext generation.
    pub seq: Option<u32>,
    /// DOS/NTFS file attribute bits (read-only, hidden, system, archive, …).
    pub attributes: Option<u32>,
}

impl Metadata {
    /// Minimal metadata for a regular, allocated file.
    pub fn new(inode: InodeId, kind: FileKind, size: u64) -> Self {
        Self {
            inode,
            kind,
            flags: MetaFlags::ALLOCATED | MetaFlags::USED,
            size,
            times: Times::default(),
            fn_times: None,
            mode: None,
            uid: None,
            gid: None,
            link_count: 1,
            seq: None,
            attributes: None,
        }
    }

    pub fn is_allocated(&self) -> bool {
        self.flags.contains(MetaFlags::ALLOCATED)
    }
}

/// A directory entry (a *name*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    /// Alternate short (8.3) name.
    pub short_name: Option<String>,
    pub inode: InodeId,
    /// NTFS: sequence number the name expects; mismatch ⇒ metadata reused.
    pub seq: Option<u32>,
    /// Type according to the directory entry.
    pub kind: FileKind,
    /// The name itself is allocated (not a deleted entry).
    pub allocated: bool,
}

/// Category of a data stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// The default (unnamed) data stream.
    Data,
    /// NTFS alternate data stream.
    AlternateData,
    /// HFS+ resource fork.
    ResourceFork,
    /// Extended attribute exposed as content.
    ExtendedAttribute,
}

/// Describes one content stream of an inode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub id: u32,
    pub name: Option<String>,
    pub kind: StreamKind,
    pub size: u64,
}

impl StreamInfo {
    pub fn default_data(size: u64) -> Self {
        Self {
            id: 0,
            name: None,
            kind: StreamKind::Data,
            size,
        }
    }
}

/// How an extent stores data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtentKind {
    /// Stored in filesystem blocks at `physical_offset`.
    Allocated,
    /// A hole that reads as zeros.
    Sparse,
    /// Stored inside a metadata record (NTFS resident data, ext inline data).
    Resident,
}

/// A run of a stream's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Extent {
    /// Offset within the stream.
    pub logical_offset: u64,
    /// Byte offset within the filesystem (meaningless for `Sparse`).
    pub physical_offset: u64,
    pub len: u64,
    pub kind: ExtentKind,
}

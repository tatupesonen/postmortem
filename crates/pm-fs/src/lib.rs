//! Filesystem abstraction.
//!
//! Every filesystem parser (`pm-fs-fat`, `pm-fs-ntfs`, …) implements
//! [`FileSystem`]. The model follows The Sleuth Kit's split between *names*
//! (directory entries, which may be deleted) and *metadata* (inodes / MFT
//! records), because forensic analysis needs both views:
//!
//! * a deleted name may still point at metadata that has been reused;
//! * metadata may exist with no name pointing at it (orphans).
//!
//! File content is exposed per *stream* (NTFS alternate data streams, HFS+
//! resource forks) as a [`SharedSource`].

mod types;
pub mod walk;

use pm_core::SharedSource;
use thiserror::Error;

pub use types::{
    DirEntry, Extent, ExtentKind, FileKind, FsInfo, FsKind, InodeId, MetaFlags, Metadata,
    StreamInfo, StreamKind, Times,
};

/// Errors from filesystem parsing.
#[derive(Debug, Error)]
pub enum FsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt {fs} structure: {reason}")]
    Corrupt { fs: FsKind, reason: String },
    #[error("inode {0} not found")]
    NotFound(InodeId),
    #[error("inode {0} is not a directory")]
    NotADirectory(InodeId),
    #[error("stream {stream} not found on inode {inode}")]
    NoSuchStream { inode: InodeId, stream: u32 },
    #[error("{0}")]
    Unsupported(String),
}

impl From<pm_core::Truncated> for FsError {
    fn from(t: pm_core::Truncated) -> Self {
        Self::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, t))
    }
}

pub type Result<T, E = FsError> = std::result::Result<T, E>;

/// A read-only, forensically complete view of a filesystem.
///
/// Implementations must be `Send + Sync`: ingest reads files from many
/// threads concurrently. Methods take `&self`; any caches use interior
/// mutability.
pub trait FileSystem: Send + Sync + std::fmt::Debug {
    /// Filesystem type and geometry.
    fn info(&self) -> &FsInfo;

    /// Root directory inode.
    fn root(&self) -> InodeId {
        self.info().root_inode
    }

    /// Metadata for `inode`, whether allocated or not.
    fn metadata(&self, inode: InodeId) -> Result<Metadata>;

    /// Entries of directory `inode`, including deleted entries that can still
    /// be recovered. `.` and `..` are omitted.
    fn read_dir(&self, inode: InodeId) -> Result<Vec<DirEntry>>;

    /// Data streams of `inode`. The default is a single unnamed data stream.
    fn streams(&self, inode: InodeId) -> Result<Vec<StreamInfo>> {
        let m = self.metadata(inode)?;
        Ok(vec![StreamInfo::default_data(m.size)])
    }

    /// Opens a stream's content. `stream = None` selects the default stream.
    ///
    /// For deleted files this returns the best-effort recovered content.
    fn open(&self, inode: InodeId, stream: Option<u32>) -> Result<SharedSource>;

    /// Physical layout of a stream (for the "file layout" view, carving and
    /// unallocated-space accounting). Offsets are relative to the start of
    /// the filesystem.
    fn extents(&self, _inode: InodeId, _stream: Option<u32>) -> Result<Vec<Extent>> {
        Ok(Vec::new())
    }

    /// Symlink target, if `inode` is a symbolic link.
    fn read_link(&self, _inode: InodeId) -> Result<Option<String>> {
        Ok(None)
    }

    /// Unallocated block ranges as `(first_block, block_count)`, coalesced and
    /// ascending.
    fn unallocated_blocks(&self) -> Result<Vec<(u64, u64)>> {
        Ok(Vec::new())
    }

    /// Metadata entries not reachable from any directory (TSK `$OrphanFiles`).
    fn orphans(&self) -> Result<Vec<InodeId>> {
        Ok(Vec::new())
    }
}

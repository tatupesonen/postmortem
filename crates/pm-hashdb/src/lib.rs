//! Hash sets (Autopsy's hash databases).
//!
//! Every format Autopsy reads through Autopsy is supported: the Autopsy SQLite
//! `.kdb` (read and write), NSRL RDS v2 text and RDS v3 SQLite, `md5sum`,
//! HashKeeper, EnCase and bare Autopsy `.idx` indexes. Non-`kdb` sets are
//! looked up through a sorted on-disk index built once
//! ([`HashDb::build_index`]), so even the full NSRL costs a few hundred
//! kilobytes of RAM and two positional reads per lookup.

mod formats;
mod index;
mod kdb;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

use parking_lot::Mutex;
use pm_core::ReadAt;
use pm_core::io::FileSource;
use thiserror::Error;

pub use formats::{Format, detect, parse_md5_hex};
pub use index::index_path;

#[derive(Debug, Error)]
pub enum HashDbError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}: not a recognised hash set format")]
    UnknownFormat(PathBuf),
    #[error("{0}: hash set must be indexed first")]
    NeedsIndex(PathBuf),
    #[error("{0} already exists")]
    Exists(PathBuf),
    #[error("hash set is read-only")]
    ReadOnly,
}

impl HashDbError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

pub type Result<T, E = HashDbError> = std::result::Result<T, E>;

/// A lookup hit with whatever the set records about the hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashHit {
    pub md5: [u8; 16],
    pub names: Vec<String>,
    pub comments: Vec<String>,
}

#[derive(Debug)]
enum Backend {
    Kdb(kdb::Kdb),
    Index {
        index: index::SortedIndex,
        /// Source handle for resolving names (text formats).
        source: Option<FileSource>,
        /// NSRL v3 connection for resolving names.
        nsrl3: Option<Mutex<rusqlite::Connection>>,
    },
}

/// An open hash set.
#[derive(Debug)]
pub struct HashDb {
    path: PathBuf,
    format: Format,
    backend: Backend,
}

impl HashDb {
    /// Creates an empty, updateable `.kdb` set.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Ok(Self {
            backend: Backend::Kdb(kdb::Kdb::create(&path)?),
            format: Format::Kdb,
            path,
        })
    }

    /// Opens a set. Formats other than `.kdb` need a current index
    /// ([`HashDbError::NeedsIndex`] otherwise; see [`Self::build_index`]).
    /// `.kdb` sets open writable unless `read_only`.
    pub fn open(path: impl AsRef<Path>, read_only: bool) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let format = detect(&path)?;
        let backend = match format {
            Format::Kdb => Backend::Kdb(kdb::Kdb::open(&path, !read_only)?),
            Format::NsrlV3 => Backend::Index {
                index: index::SortedIndex::open(&path, format)?,
                source: None,
                nsrl3: Some(Mutex::new(rusqlite::Connection::open_with_flags(
                    &path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )?)),
            },
            _ => Backend::Index {
                index: index::SortedIndex::open(&path, format)?,
                source: Some(FileSource::open(&path).map_err(|e| HashDbError::io(&path, e))?),
                nsrl3: None,
            },
        };
        Ok(Self {
            path,
            format,
            backend,
        })
    }

    /// Opens a set, building its index first if needed.
    pub fn open_or_index(path: impl AsRef<Path>, progress: &AtomicU64) -> Result<Self> {
        match Self::open(&path, true) {
            Err(HashDbError::NeedsIndex(_)) => {
                Self::build_index(&path, progress)?;
                Self::open(path, true)
            }
            r => r,
        }
    }

    /// (Re)builds the sorted index of a non-`kdb` set; `progress` counts
    /// entries read. Returns the number of indexed hashes.
    pub fn build_index(path: impl AsRef<Path>, progress: &AtomicU64) -> Result<u64> {
        Self::build_index_chunked(path, progress, index::DEFAULT_CHUNK)
    }

    #[doc(hidden)]
    pub fn build_index_chunked(
        path: impl AsRef<Path>,
        progress: &AtomicU64,
        chunk: usize,
    ) -> Result<u64> {
        let path = path.as_ref();
        let format = detect(path)?;
        if !format.needs_index() {
            return Ok(0);
        }
        index::build(path, format, chunk.max(1), progress)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Name for display: the EnCase header name, else the file stem.
    pub fn display_name(&self) -> String {
        let stem = || {
            self.path
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
        };
        if self.format == Format::EnCase
            && let Backend::Index {
                source: Some(src), ..
            } = &self.backend
        {
            let mut buf = [0u8; 80];
            if src.read_exact_at(formats::ENCASE_NAME, &mut buf).is_ok() {
                let units: Vec<u16> = buf
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .take_while(|&u| u != 0)
                    .collect();
                let name = String::from_utf16_lossy(&units);
                if !name.trim().is_empty() {
                    return name.trim().to_owned();
                }
            }
        }
        stem()
    }

    /// Whether hashes can be added (`.kdb` opened writable).
    pub fn is_updateable(&self) -> bool {
        matches!(&self.backend, Backend::Kdb(k) if k.writable())
    }

    /// Number of hashes (records for indexed sets).
    pub fn len(&self) -> Result<u64> {
        match &self.backend {
            Backend::Kdb(k) => k.len(),
            Backend::Index { index, .. } => Ok(index.len()),
        }
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Fast membership test (Autopsy's `lookupMD5Quick`).
    pub fn contains(&self, md5: &[u8; 16]) -> Result<bool> {
        match &self.backend {
            Backend::Kdb(k) => k.contains(md5),
            Backend::Index { index, .. } => Ok(!index.find(md5)?.is_empty()),
        }
    }

    /// Full lookup with names and comments (Autopsy's `lookupMD5`).
    pub fn lookup(&self, md5: &[u8; 16]) -> Result<Option<HashHit>> {
        match &self.backend {
            Backend::Kdb(k) => k.lookup(md5),
            Backend::Index {
                index,
                source,
                nsrl3,
            } => {
                let locs = index.find(md5)?;
                if locs.is_empty() {
                    return Ok(None);
                }
                let mut hit = HashHit {
                    md5: *md5,
                    names: Vec::new(),
                    comments: Vec::new(),
                };
                for loc in locs {
                    let (name, comment) = self.resolve(source.as_ref(), nsrl3.as_ref(), loc)?;
                    if let Some(n) = name.filter(|n| !hit.names.contains(n)) {
                        hit.names.push(n);
                    }
                    if let Some(c) = comment.filter(|c| !hit.comments.contains(c)) {
                        hit.comments.push(c);
                    }
                }
                Ok(Some(hit))
            }
        }
    }

    /// Adds a hash to an updateable `.kdb` set.
    pub fn add(
        &self,
        md5: &[u8; 16],
        sha1: Option<&[u8]>,
        sha256: Option<&[u8]>,
        name: Option<&str>,
        comment: Option<&str>,
    ) -> Result<()> {
        match &self.backend {
            Backend::Kdb(k) => k.add(md5, sha1, sha256, name, comment),
            Backend::Index { .. } => Err(HashDbError::ReadOnly),
        }
    }

    /// Name and comment stored at a locator.
    fn resolve(
        &self,
        source: Option<&FileSource>,
        nsrl3: Option<&Mutex<rusqlite::Connection>>,
        loc: u64,
    ) -> Result<(Option<String>, Option<String>)> {
        if let Some(conn) = nsrl3 {
            let name: Option<String> = conn
                .lock()
                .query_row(
                    "SELECT file_name FROM FILE WHERE rowid = ?1",
                    [loc.cast_signed()],
                    |r| r.get(0),
                )
                .ok();
            return Ok((name, None));
        }
        let Some(src) = source else {
            return Ok((None, None));
        };
        if matches!(self.format, Format::EnCase | Format::HfindIdx) {
            return Ok((None, None));
        }
        // Read the line starting at `loc`.
        let mut line = Vec::new();
        let mut off = loc;
        let mut buf = [0u8; 1024];
        loop {
            let n = src
                .read_at(off, &mut buf)
                .map_err(|e| HashDbError::io(&self.path, e))?;
            let chunk = buf.get(..n).unwrap_or_default();
            if let Some(i) = chunk.iter().position(|&b| b == b'\n') {
                line.extend_from_slice(chunk.get(..i).unwrap_or_default());
                break;
            }
            line.extend_from_slice(chunk);
            if n == 0 || line.len() > 64 * 1024 {
                break;
            }
            off += n as u64;
        }
        let text = String::from_utf8_lossy(&line);
        let layout = if self.format == Format::NsrlV2 {
            let mut head = [0u8; 512];
            let n = src.read_at(0, &mut head).unwrap_or(0);
            let first = String::from_utf8_lossy(head.get(..n).unwrap_or_default());
            formats::NsrlLayout::from_header(first.lines().next().unwrap_or_default())
        } else {
            formats::NsrlLayout::default()
        };
        Ok(formats::parse_line(self.format, layout, &text)
            .map_or((None, None), |e| (e.name, e.comment)))
    }
}

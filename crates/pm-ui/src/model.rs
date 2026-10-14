//! What the UI shows: data sources, tree nodes, directory rows and content.
//!
//! This is the seam between the UI and the evidence. Today it reads the
//! evidence layer directly. Once the case database exists the same types
//! are populated from SQL, so the UI code does not change.

use std::sync::Arc;

use pm_core::SharedSource;
use pm_evidence::Evidence;
use pm_fs::{FileKind, FileSystem, InodeId, Metadata, StreamKind};

/// Index of a data source in the session.
pub(crate) type DsId = usize;

/// A loaded data source (one disk image).
#[derive(Debug, Clone)]
pub struct DataSource {
    pub id: DsId,
    pub name: String,
    pub evidence: Evidence,
}

impl DataSource {
    pub fn fs(&self, idx: usize) -> Option<&Arc<dyn FileSystem>> {
        self.evidence.filesystems.get(idx).map(|f| &f.fs)
    }
}

/// A node in the directory tree (also the "location" in navigation history).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeId {
    DataSource(DsId),
    /// A volume-system slot (partition, unallocated or meta).
    Volume {
        ds: DsId,
        addr: u32,
    },
    /// A directory within filesystem `fs` of data source `ds`.
    Dir {
        ds: DsId,
        fs: usize,
        inode: InodeId,
        path: Arc<str>,
    },
    /// The `$OrphanFiles` virtual directory of a filesystem.
    Orphans {
        ds: DsId,
        fs: usize,
    },
    /// Results of one artifact type in the case database.
    Results {
        type_id: i64,
    },
}

impl NodeId {
    /// The data source this node belongs to (`None` for case-wide nodes).
    pub fn ds(&self) -> Option<DsId> {
        match self {
            Self::DataSource(ds)
            | Self::Volume { ds, .. }
            | Self::Dir { ds, .. }
            | Self::Orphans { ds, .. } => Some(*ds),
            Self::Results { .. } => None,
        }
    }
}

/// One row of a directory listing.
#[derive(Debug, Clone)]
pub struct FileRow {
    pub name: String,
    pub inode: InodeId,
    /// `Some(id)` for an alternate data stream row (`file:stream`).
    pub stream: Option<u32>,
    pub kind: FileKind,
    /// The *name* is allocated (Autopsy "Flags(Dir)").
    pub name_allocated: bool,
    pub meta: Option<Metadata>,
    /// Size of this row's stream (ADS rows differ from the file size).
    pub size: u64,
    pub path: String,
}

impl FileRow {
    /// Builds a row for a known inode (used when navigating from results
    /// such as the timeline rather than a directory listing).
    pub(crate) fn from_fs(
        fs: &dyn FileSystem,
        inode: InodeId,
        path: &str,
        name_allocated: bool,
    ) -> Self {
        let meta = fs.metadata(inode).ok();
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        Self {
            name,
            inode,
            stream: None,
            kind: meta.as_ref().map_or(FileKind::Unknown, |m| m.kind),
            name_allocated,
            size: meta.as_ref().map_or(0, |m| m.size),
            meta,
            path: path.to_owned(),
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Directory && self.stream.is_none()
    }

    /// Autopsy "Flags(Meta)": allocated metadata.
    pub fn meta_allocated(&self) -> bool {
        self.meta.as_ref().is_some_and(Metadata::is_allocated)
    }

    pub fn deleted(&self) -> bool {
        !self.name_allocated || !self.meta_allocated()
    }

    pub fn extension(&self) -> &str {
        match self.name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && !ext.contains(':') => ext,
            _ => "",
        }
    }
}

fn row_for(fs: &dyn FileSystem, e: &pm_fs::DirEntry, parent: &str) -> Vec<FileRow> {
    let meta = pm_fs::entry_metadata(fs, e).ok();
    let path = if parent.ends_with('/') {
        format!("{parent}{}", e.name)
    } else {
        format!("{parent}/{}", e.name)
    };
    let kind = meta.as_ref().map_or(e.kind, |m| m.kind);
    let size = meta.as_ref().map_or(0, |m| m.size);
    let mut rows = vec![FileRow {
        name: e.name.clone(),
        inode: e.inode,
        stream: None,
        kind,
        name_allocated: e.allocated,
        meta: meta.clone(),
        size,
        path: path.clone(),
    }];
    // NTFS alternate data streams appear as their own rows, as in Autopsy.
    if let Ok(streams) = fs.streams(e.inode) {
        for s in streams.into_iter().filter(|s| s.kind != StreamKind::Data) {
            let sname = s.name.unwrap_or_default();
            rows.push(FileRow {
                name: format!("{}:{sname}", e.name),
                inode: e.inode,
                stream: Some(s.id),
                kind: FileKind::Regular,
                name_allocated: e.allocated,
                meta: meta.clone(),
                size: s.size,
                path: format!("{path}:{sname}"),
            });
        }
    }
    rows
}

/// Lists a directory (blocking; call from a background task).
pub(crate) fn list_dir(
    fs: &dyn FileSystem,
    inode: InodeId,
    path: &str,
) -> Result<Vec<FileRow>, String> {
    let entries = fs.read_dir(inode).map_err(|e| e.to_string())?;
    Ok(entries.iter().flat_map(|e| row_for(fs, e, path)).collect())
}

/// Lists `$OrphanFiles` (blocking; may scan unallocated space).
pub(crate) fn list_orphans(fs: &dyn FileSystem) -> Result<Vec<FileRow>, String> {
    let entries = fs.orphans().map_err(|e| e.to_string())?;
    Ok(entries
        .iter()
        .flat_map(|e| row_for(fs, e, "/$OrphanFiles"))
        .collect())
}

/// A file (or stream) selected for viewing.
#[derive(Debug, Clone)]
pub struct Content {
    pub fs: Arc<dyn FileSystem>,
    pub row: FileRow,
}

impl PartialEq for Content {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.fs, &other.fs)
            && self.row.inode == other.row.inode
            && self.row.stream == other.row.stream
    }
}

impl Content {
    pub fn source(&self) -> Result<SharedSource, String> {
        self.fs
            .open(self.row.inode, self.row.stream)
            .map_err(|e| e.to_string())
    }
}

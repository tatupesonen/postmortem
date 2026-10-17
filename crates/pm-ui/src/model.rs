//! What the UI shows: data sources, tree nodes, directory rows and content.
//!
//! Browsing reads the case database (tree nodes are case ids, rows carry
//! their `FileRecord`). The analysis tools still on the in-memory file
//! index (Gallery, Discovery, Summary, Timeline) build rows and content from
//! opened file systems until they move to indexed queries (WP-15).

use std::sync::Arc;

use pm_case::Case;
use pm_casedb::FileRecord;
use pm_core::SharedSource;
use pm_evidence::Evidence;
use pm_fs::{FileKind, FileSystem, InodeId, Metadata};

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
/// Ids are case-database ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeId {
    DataSource(i64),
    /// A volume-system slot (partition or unallocated space).
    Volume {
        ds: i64,
        volume: i64,
    },
    /// A directory or virtual directory (`$OrphanFiles`, `$Unalloc`, …),
    /// by file id.
    Dir {
        ds: i64,
        file: i64,
    },
    /// Results of one artifact type in the case database.
    Results {
        type_id: i64,
    },
    /// A node of the Views tree (file types, deleted files, sizes).
    View(crate::views::View),
    /// Tags › `<tag name>` › File Tags.
    TagFiles(i64),
    /// Tags › `<tag name>` › Result Tags.
    TagResults(i64),
    /// Score › Bad Items (`true`) / Suspicious Items.
    Score {
        bad: bool,
    },
    OsAccounts,
}

impl NodeId {
    /// The data source this node belongs to (`None` for case-wide nodes).
    pub fn ds(&self) -> Option<i64> {
        match self {
            Self::DataSource(ds) | Self::Volume { ds, .. } | Self::Dir { ds, .. } => Some(*ds),
            Self::Results { .. }
            | Self::View(_)
            | Self::TagFiles(_)
            | Self::TagResults(_)
            | Self::Score { .. }
            | Self::OsAccounts => None,
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
    /// The case-database row, for rows listed from the case.
    pub file: Option<Arc<FileRecord>>,
    /// Score and tags (Autopsy's S and C columns), for case rows.
    pub annotations: Option<Arc<Annotations>>,
}

/// A tag on a listed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowTag {
    pub id: i64,
    pub tag_name_id: i64,
    pub name: String,
    pub comment: String,
}

/// Aggregate score and tags of a listed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotations {
    pub score: pm_casedb::Significance,
    pub tags: Vec<RowTag>,
}

impl Annotations {
    /// Autopsy's C column: tags exist, and one has a comment.
    pub fn comment_status(&self) -> CommentStatus {
        if self.tags.is_empty() {
            CommentStatus::None
        } else if self.tags.iter().any(|t| !t.comment.trim().is_empty()) {
            CommentStatus::TagComment
        } else {
            CommentStatus::TagNoComment
        }
    }
}

/// Autopsy `HasCommentStatus` (without the Central Repository states yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommentStatus {
    None,
    TagNoComment,
    TagComment,
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
            file: None,
            annotations: None,
        }
    }

    /// Case-database file id of the row, if it came from the case.
    pub fn file_id(&self) -> Option<i64> {
        self.file.as_ref().map(|f| f.id)
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

/// Where a viewed file's content comes from.
#[derive(Debug, Clone)]
pub enum ContentOrigin {
    /// A file of an opened file system (tools still on the in-memory
    /// index).
    Fs(Arc<dyn FileSystem>),
    /// A file of the case database (any origin: file system, carved,
    /// derived, unallocated space).
    Case(Arc<Case>),
}

/// A file (or stream) selected for viewing.
#[derive(Debug, Clone)]
pub struct Content {
    pub origin: ContentOrigin,
    pub row: FileRow,
}

impl PartialEq for Content {
    fn eq(&self, other: &Self) -> bool {
        match (&self.origin, &other.origin) {
            (ContentOrigin::Case(a), ContentOrigin::Case(b)) => {
                Arc::ptr_eq(a, b) && self.row.file_id() == other.row.file_id()
            }
            (ContentOrigin::Fs(a), ContentOrigin::Fs(b)) => {
                Arc::ptr_eq(a, b)
                    && self.row.inode == other.row.inode
                    && self.row.stream == other.row.stream
            }
            _ => false,
        }
    }
}

impl Content {
    pub fn from_fs(fs: Arc<dyn FileSystem>, row: FileRow) -> Self {
        Self {
            origin: ContentOrigin::Fs(fs),
            row,
        }
    }

    /// Content of a case file (the row must carry its [`FileRecord`]).
    pub fn from_case(case: Arc<Case>, row: FileRow) -> Self {
        Self {
            origin: ContentOrigin::Case(case),
            row,
        }
    }

    pub fn source(&self) -> Result<SharedSource, String> {
        match &self.origin {
            ContentOrigin::Fs(fs) => fs
                .open(self.row.inode, self.row.stream)
                .map_err(|e| e.to_string()),
            ContentOrigin::Case(case) => {
                let f = self.row.file.as_ref().ok_or("no case file for this row")?;
                case.content(f).map_err(|e| e.to_string())
            }
        }
    }

    /// The file system holding the file, opening the case's evidence if
    /// needed (blocking on first use per data source; `Case` caches it).
    pub fn filesystem(&self) -> Option<Arc<dyn FileSystem>> {
        match &self.origin {
            ContentOrigin::Fs(fs) => Some(Arc::clone(fs)),
            ContentOrigin::Case(case) => {
                let f = self.row.file.as_ref()?;
                if !matches!(
                    f.origin,
                    pm_casedb::Origin::FileSystem | pm_casedb::Origin::Slack
                ) {
                    return None;
                }
                case.filesystem(f.fs_id?).ok()
            }
        }
    }

    /// The case and file record, for case-backed content.
    pub fn case_file(&self) -> Option<(&Arc<Case>, &FileRecord)> {
        match &self.origin {
            ContentOrigin::Case(case) => self.row.file.as_deref().map(|f| (case, f)),
            ContentOrigin::Fs(_) => None,
        }
    }
}

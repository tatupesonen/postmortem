//! Autopsy's "Views" tree: file types by extension and by MIME type,
//! deleted files and file-size buckets. Everything is an indexed query on
//! the case database (ADR-014), never a scan of the evidence.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pm_case::Case;
use pm_casedb::{FileKindCode, FileQuery, FileRecord};
use pm_fs::{FileKind, MetaFlags, Metadata, Times};

use crate::model::FileRow;
use crate::task::Task;

/// Rows loaded per view (the table is not paged yet).
pub(crate) const MAX_ROWS: usize = 50_000;
const REFRESH: Duration = Duration::from_secs(3);

/// Autopsy `FileTypeExtensions` groups (By Extension).
pub(crate) const EXT_GROUPS: &[(&str, &[&str])] = &[
    (
        "Images",
        &[
            "jpg", "jpeg", "png", "psd", "nef", "tiff", "bmp", "tec", "tif", "webp",
        ],
    ),
    (
        "Videos",
        &[
            "aaf", "3gp", "asf", "avi", "m1v", "m2v", "m4v", "mp4", "mov", "mpeg", "mpg", "mpe",
            "rm", "wmv", "mpv", "flv", "swf",
        ],
    ),
    (
        "Audio",
        &[
            "aiff", "aif", "flac", "wav", "m4a", "ape", "wma", "mp2", "mp1", "mp3", "aac", "mp4",
            "m4p", "m1a", "m2a", "m4r", "mpa", "m3u", "mid", "midi", "ogg",
        ],
    ),
    (
        "Archives",
        &[
            "zip", "rar", "7zip", "7z", "arj", "tar", "gzip", "bzip", "bzip2", "cab", "jar",
            "cpio", "ar", "gz", "tgz", "bz2",
        ],
    ),
    ("Databases", &["db", "db3", "sqlite", "sqlite3"]),
    (
        "Executable",
        &[
            "exe", "msi", "cmd", "com", "bat", "reg", "scr", "dll", "ini",
        ],
    ),
];

/// Autopsy `DocumentFilter` sub-groups (By Extension › Documents).
pub(crate) const DOC_GROUPS: &[(&str, &[&str])] = &[
    ("HTML", &["htm", "html"]),
    (
        "Office",
        &["doc", "docx", "odt", "xls", "xlsx", "ppt", "pptx"],
    ),
    ("PDF", &["pdf"]),
    ("Plain Text", &["txt"]),
    ("Rich Text", &["rtf"]),
];

/// Autopsy `FileSize` buckets (decimal megabytes).
pub(crate) const SIZE_BUCKETS: &[(&str, u64, Option<u64>)] = &[
    ("50 - 200MB", 50_000_000, Some(199_999_999)),
    ("200MB - 1GB", 200_000_000, Some(999_999_999)),
    ("1GB+", 1_000_000_000, None),
];

/// One node of the Views tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum View {
    /// Index into [`EXT_GROUPS`].
    Ext(usize),
    /// Index into [`DOC_GROUPS`].
    Doc(usize),
    Mime(String),
    /// Deleted files; `true` = file systems only, `false` = all.
    Deleted(bool),
    /// Index into [`SIZE_BUCKETS`].
    Size(usize),
}

impl View {
    pub(crate) fn title(&self) -> String {
        match self {
            Self::Ext(i) => EXT_GROUPS.get(*i).map_or("", |g| g.0).to_owned(),
            Self::Doc(i) => format!("Documents › {}", DOC_GROUPS.get(*i).map_or("", |g| g.0)),
            Self::Mime(m) => m.clone(),
            Self::Deleted(true) => "Deleted Files › File System".into(),
            Self::Deleted(false) => "Deleted Files › All".into(),
            Self::Size(i) => format!("File Size › {}", SIZE_BUCKETS.get(*i).map_or("", |b| b.0)),
        }
    }

    /// The query behind the view.
    pub(crate) fn query(&self, hide_known: bool) -> FileQuery {
        let ext = |l: &[&str]| l.iter().map(|e| (*e).to_owned()).collect();
        let mut q = FileQuery {
            hide_known,
            ..FileQuery::default()
        };
        match self {
            Self::Ext(i) => {
                q.regular_only = true;
                q.extensions = EXT_GROUPS.get(*i).map_or_else(Vec::new, |g| ext(g.1));
            }
            Self::Doc(i) => {
                q.regular_only = true;
                q.extensions = DOC_GROUPS.get(*i).map_or_else(Vec::new, |g| ext(g.1));
            }
            Self::Mime(m) => {
                q.regular_only = true;
                q.mime = Some(m.clone());
            }
            Self::Deleted(fs_only) => {
                q.deleted_only = true;
                q.fs_only = *fs_only;
                q.regular_only = true;
            }
            Self::Size(i) => {
                q.regular_only = true;
                if let Some(b) = SIZE_BUCKETS.get(*i) {
                    q.min_size = Some(b.1);
                    q.max_size = b.2;
                }
            }
        }
        q
    }
}

/// Counts for every view node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ViewCounts {
    pub(crate) ext: Vec<u64>,
    pub(crate) doc: Vec<u64>,
    pub(crate) mime: Vec<(String, u64)>,
    pub(crate) deleted: [u64; 2],
    pub(crate) size: Vec<u64>,
}

fn load_counts(case: &Case, hide_known: bool) -> Result<ViewCounts, String> {
    let db = case.db();
    let count = |v: View| {
        db.count_files(&v.query(hide_known))
            .map_err(|e| e.to_string())
    };
    Ok(ViewCounts {
        ext: (0..EXT_GROUPS.len())
            .map(|i| count(View::Ext(i)))
            .collect::<Result<_, _>>()?,
        doc: (0..DOC_GROUPS.len())
            .map(|i| count(View::Doc(i)))
            .collect::<Result<_, _>>()?,
        mime: db.mime_counts(None).map_err(|e| e.to_string())?,
        deleted: [count(View::Deleted(true))?, count(View::Deleted(false))?],
        size: (0..SIZE_BUCKETS.len())
            .map(|i| count(View::Size(i)))
            .collect::<Result<_, _>>()?,
    })
}

/// View counts, refreshed in the background.
#[derive(Debug, Default)]
pub(crate) struct Counts {
    task: Option<Task<Result<ViewCounts, String>>>,
    current: Arc<ViewCounts>,
    refreshed: Option<Instant>,
}

impl Counts {
    pub(crate) fn get(
        &mut self,
        ctx: &egui::Context,
        case: Option<&Arc<Case>>,
        live: bool,
        hide_known: bool,
    ) -> Arc<ViewCounts> {
        if let Some(t) = &mut self.task
            && let Some(r) = t.poll()
        {
            if let Ok(v) = r {
                self.current = Arc::new(v.clone());
            }
            self.task = None;
            self.refreshed = Some(Instant::now());
        }
        let stale = self
            .refreshed
            .is_none_or(|t| live && t.elapsed() >= REFRESH);
        if self.task.is_none()
            && stale
            && let Some(case) = case
        {
            let case = Arc::clone(case);
            self.task = Some(Task::spawn(ctx, move || load_counts(&case, hide_known)));
        }
        Arc::clone(&self.current)
    }

    pub(crate) fn invalidate(&mut self) {
        self.refreshed = None;
        self.task = None;
    }
}

/// A view's files, as table rows plus their case records.
#[derive(Debug, Clone, Default)]
pub(crate) struct ViewRows {
    pub(crate) rows: Vec<FileRow>,
    pub(crate) records: Vec<FileRecord>,
    pub(crate) truncated: bool,
}

pub(crate) fn load_rows(case: &Case, view: &View, hide_known: bool) -> Result<ViewRows, String> {
    let q = view.query(hide_known);
    let mut records = Vec::new();
    let mut after = 0;
    while records.len() <= MAX_ROWS {
        let page = case
            .db()
            .query_files(&q, after, 5_000)
            .map_err(|e| e.to_string())?;
        let Some(last) = page.last() else { break };
        after = last.id;
        records.extend(page);
    }
    let truncated = records.len() > MAX_ROWS;
    records.truncate(MAX_ROWS);
    let mut rows: Vec<FileRow> = records.iter().map(row_of).collect();
    crate::tags_ui::annotate(case, &mut rows)?;
    Ok(ViewRows {
        rows,
        records,
        truncated,
    })
}

fn kind_of(k: FileKindCode) -> FileKind {
    match k {
        FileKindCode::Regular => FileKind::Regular,
        FileKindCode::Directory => FileKind::Directory,
        FileKindCode::Symlink => FileKind::Symlink,
        FileKindCode::CharDevice => FileKind::CharDevice,
        FileKindCode::BlockDevice => FileKind::BlockDevice,
        FileKindCode::Fifo => FileKind::Fifo,
        FileKindCode::Socket => FileKind::Socket,
        FileKindCode::Virtual => FileKind::Virtual,
        FileKindCode::Unknown => FileKind::Unknown,
    }
}

/// A table row from a case record (no evidence access).
pub(crate) fn row_of(f: &FileRecord) -> FileRow {
    let t = |i: usize| {
        f.times
            .get(i)
            .copied()
            .flatten()
            .and_then(|(s, ns)| jiff::Timestamp::new(s, i32::try_from(ns).unwrap_or(0)).ok())
    };
    let mut meta = Metadata::new(f.inode.unwrap_or(0), kind_of(f.kind), f.size);
    meta.flags = MetaFlags::from_bits_truncate(u16::try_from(f.meta_flags & 0xFFFF).unwrap_or(0));
    if f.meta_alloc {
        meta.flags |= MetaFlags::ALLOCATED;
    } else {
        meta.flags.remove(MetaFlags::ALLOCATED);
    }
    meta.times = Times {
        modified: t(0),
        accessed: t(1),
        changed: t(2),
        created: t(3),
    };
    meta.mode = f.mode;
    meta.uid = f.uid;
    meta.gid = f.gid;
    meta.seq = f.seq;
    meta.attributes = f.attributes;
    FileRow {
        name: f.name.clone(),
        inode: f.inode.unwrap_or(0),
        stream: f.stream,
        kind: kind_of(f.kind),
        name_allocated: f.name_alloc,
        meta: Some(meta),
        size: f.size,
        path: f.path(),
        file: Some(Arc::new(f.clone())),
        annotations: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_match_autopsy_definitions() {
        let q = View::Ext(0).query(true);
        assert!(q.regular_only && q.hide_known);
        assert!(q.extensions.contains(&"jpg".to_owned()));
        let q = View::Size(0).query(false);
        assert_eq!(
            (q.min_size, q.max_size),
            (Some(50_000_000), Some(199_999_999))
        );
        let q = View::Size(2).query(false);
        assert_eq!((q.min_size, q.max_size), (Some(1_000_000_000), None));
        assert!(View::Deleted(true).query(false).fs_only);
        assert!(!View::Deleted(false).query(false).fs_only);
        assert_eq!(
            View::Mime("image/png".into()).query(false).mime.as_deref(),
            Some("image/png")
        );
        assert_eq!(View::Doc(1).title(), "Documents › Office");
    }

    #[test]
    fn rows_from_records() {
        let f = FileRecord {
            id: 1,
            ds_id: 1,
            fs_id: Some(1),
            parent_id: Some(0),
            origin: pm_casedb::Origin::FileSystem,
            inode: Some(42),
            stream: None,
            name: "a.jpg".into(),
            parent_path: "/x/".into(),
            kind: FileKindCode::Regular,
            name_alloc: false,
            meta_alloc: false,
            meta_flags: 2,
            size: 10,
            times: [Some((1_600_000_000, 5)), None, None, None],
            mode: None,
            uid: None,
            gid: None,
            seq: None,
            attributes: None,
            extension: "jpg".into(),
            mime: None,
            md5: None,
            sha1: None,
            sha256: None,
            known: pm_casedb::Known::Unknown,
            derived_from: None,
            local_path: None,
        };
        let r = row_of(&f);
        assert_eq!(r.path, "/x/a.jpg");
        assert!(r.deleted());
        assert_eq!(
            r.meta.unwrap().times.modified.unwrap().as_second(),
            1_600_000_000
        );
    }
}

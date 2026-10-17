//! Derived, local and carved files and the shared file-id allocator.
//!
//! Derived files are ordinary `files` rows: `origin` carved/derived/local,
//! `derived_from` the source file, `local_path` (relative to the case
//! folder) for extracted bytes, `layout` for carved byte ranges.
//!
//! Every writer that inserts `files` rows — the data-source loader, the
//! ingest writer, modules adding derived or carved files — takes ids from
//! one [`FileIds`] allocator per open database. Ids can therefore be handed
//! out before rows are written (a directory tree of archive members can
//! reference its parents before the single writer inserts them) without
//! two writers ever choosing the same id.

use std::ops::Range;
use std::sync::atomic::{AtomicI64, Ordering};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{CaseDb, FILE_COLS, FileKindCode, FileRecord, Known, NewFile, Origin, Result};

/// Hands out unused `files.id` values (Autopsy's object-id sequence).
///
/// Initialised from `MAX(id) + 1` when the database is opened; ids are
/// never reused within a session, so gaps are normal (cancelled loads,
/// reserved but unused ids).
#[derive(Debug)]
pub struct FileIds(AtomicI64);

impl FileIds {
    pub(crate) fn load(conn: &Connection) -> Result<Self> {
        let next: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM files", [], |r| {
            r.get(0)
        })?;
        Ok(Self(AtomicI64::new(next.max(1))))
    }

    /// One new id.
    pub fn next(&self) -> i64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }

    /// `n` consecutive new ids.
    pub fn reserve(&self, n: u64) -> Range<i64> {
        let n = i64::try_from(n).unwrap_or(i64::MAX / 4);
        let start = self.0.fetch_add(n, Ordering::Relaxed);
        start..start.saturating_add(n)
    }

    /// The id the next call to [`Self::next`] returns (ids below it are
    /// taken or were handed out).
    pub fn peek(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Makes sure `id` is never handed out (a row was written with an
    /// explicit id that did not come from this allocator).
    pub(crate) fn observe(&self, id: i64) {
        self.0.fetch_max(id.saturating_add(1), Ordering::Relaxed);
    }
}

impl NewFile {
    /// A row with every optional field empty: no file system, inode,
    /// stream, times or hashes; allocated; size 0. `parent_path` must be
    /// `/`-terminated.
    pub fn new(
        ds_id: i64,
        parent_id: Option<i64>,
        parent_path: impl Into<String>,
        name: impl Into<String>,
        origin: Origin,
        kind: FileKindCode,
    ) -> Self {
        Self {
            id: None,
            ds_id,
            fs_id: None,
            parent_id,
            origin,
            inode: None,
            stream: None,
            name: name.into(),
            parent_path: parent_path.into(),
            kind,
            name_alloc: true,
            meta_alloc: true,
            meta_flags: 0,
            size: 0,
            times: [None; 4],
            mode: None,
            uid: None,
            gid: None,
            seq: None,
            attributes: None,
            derived_from: None,
            local_path: None,
            layout: None,
        }
    }

    /// The record this row becomes once inserted with `id` (no MIME type,
    /// hashes or known status yet).
    pub fn to_record(&self, id: i64) -> FileRecord {
        FileRecord {
            id,
            ds_id: self.ds_id,
            fs_id: self.fs_id,
            parent_id: self.parent_id,
            origin: self.origin,
            inode: self.inode,
            stream: self.stream,
            name: self.name.clone(),
            parent_path: self.parent_path.clone(),
            kind: self.kind,
            name_alloc: self.name_alloc,
            meta_alloc: self.meta_alloc,
            meta_flags: self.meta_flags,
            size: self.size,
            times: self.times,
            mode: self.mode,
            uid: self.uid,
            gid: self.gid,
            seq: self.seq,
            attributes: self.attributes,
            extension: crate::extension_of(&self.name),
            mime: None,
            md5: None,
            sha1: None,
            sha256: None,
            known: Known::Unknown,
            derived_from: self.derived_from,
            local_path: self.local_path.clone(),
        }
    }
}

/// Changed fields of a re-extracted derived file (Autopsy
/// `updateDerivedFile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedUpdate {
    pub size: u64,
    /// modified, accessed, changed, created.
    pub times: [Option<(i64, u32)>; 4],
    pub local_path: Option<String>,
    /// Clear the MIME type so it is detected again (Autopsy does this when
    /// the old type was `application/octet-stream`).
    pub reset_mime: bool,
}

impl CaseDb {
    /// The shared file-id allocator.
    pub fn file_ids(&self) -> &FileIds {
        &self.file_ids
    }

    /// Inserts derived/local/carved files in one transaction and returns
    /// their records (ids assigned from [`Self::file_ids`] where missing).
    /// Parents must come before their children.
    pub fn add_derived_files(&self, files: &[NewFile]) -> Result<Vec<FileRecord>> {
        self.bulk(|b| {
            let mut out = Vec::with_capacity(files.len());
            for f in files {
                let id = b.add_file(f)?;
                out.push(f.to_record(id));
            }
            Ok(out)
        })
    }

    /// Files derived from `source_id` (members of an archive, images of a
    /// document, files carved from an unallocated-space file), by id, with
    /// keyset paging.
    pub fn derived_files_of(
        &self,
        source_id: i64,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<FileRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {FILE_COLS} FROM files WHERE derived_from = ?1 AND id > ?2 ORDER BY id LIMIT {limit}"
            ))?;
            let rows = st
                .query_map(params![source_id, after_id], FileRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Whether any file has `parent_id` as its parent.
    pub fn has_children(&self, parent_id: i64) -> Result<bool> {
        self.read(|c| {
            Ok(
                c.prepare_cached("SELECT EXISTS (SELECT 1 FROM files WHERE parent_id = ?1)")?
                    .query_row([parent_id], |r| r.get(0))?,
            )
        })
    }

    /// Which of `ids` have children (archives with extracted members,
    /// files with derived files), for showing them as openable.
    pub fn with_children(&self, ids: &[i64]) -> Result<std::collections::HashSet<i64>> {
        self.read(|c| {
            let mut st =
                c.prepare_cached("SELECT EXISTS (SELECT 1 FROM files WHERE parent_id = ?1)")?;
            let mut out = std::collections::HashSet::new();
            for id in ids {
                if st.query_row([id], |r| r.get::<_, bool>(0))? {
                    out.insert(*id);
                }
            }
            Ok(out)
        })
    }

    /// Number of files whose parent is `parent_id` (e.g. carved files in
    /// a `$CarvedFiles/<n>` folder).
    pub fn count_children(&self, parent_id: i64) -> Result<u64> {
        self.read(|c| {
            let n: i64 = c
                .prepare_cached("SELECT COUNT(*) FROM files WHERE parent_id = ?1")?
                .query_row([parent_id], |r| r.get(0))?;
            Ok(u64::try_from(n).unwrap_or(0))
        })
    }

    /// The child of `parent_id` called `name` (exact match).
    pub fn child_named(&self, parent_id: i64, name: &str) -> Result<Option<FileRecord>> {
        self.read(|c| {
            Ok(c.prepare_cached(&format!(
                "SELECT {FILE_COLS} FROM files WHERE parent_id = ?1 AND name = ?2 ORDER BY id LIMIT 1"
            ))?
            .query_row(params![parent_id, name], FileRecord::from_row)
            .optional()?)
        })
    }

    /// Top-level files outside any file system (logical file sets,
    /// unallocated space image pieces, a data-source `$CarvedFiles`).
    pub fn top_level_files(&self, ds_id: i64) -> Result<Vec<FileRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {FILE_COLS} FROM files WHERE ds_id = ?1 AND parent_id IS NULL AND fs_id IS NULL ORDER BY name, id"
            ))?;
            let rows = st
                .query_map(params![ds_id], FileRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// The top-level file (no parent) of a data source named `name`.
    pub fn top_level_named(&self, ds_id: i64, name: &str) -> Result<Option<FileRecord>> {
        self.read(|c| {
            Ok(c.prepare_cached(&format!(
                "SELECT {FILE_COLS} FROM files WHERE ds_id = ?1 AND parent_id IS NULL AND name = ?2 ORDER BY id LIMIT 1"
            ))?
            .query_row(params![ds_id, name], FileRecord::from_row)
            .optional()?)
        })
    }

    /// Files of a data source's file systems with metadata address `inode`
    /// and name `name` (Autopsy's `meta_addr` + `name` lookup).
    pub fn files_by_inode(&self, ds_id: i64, inode: u64, name: &str) -> Result<Vec<i64>> {
        let inode = i64::try_from(inode).unwrap_or(i64::MAX);
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT f.id FROM file_systems s JOIN files f ON f.fs_id = s.id \
                 WHERE s.ds_id = ?1 AND f.inode = ?2 AND f.name = ?3 ORDER BY f.id",
            )?;
            let ids = st
                .query_map(params![ds_id, inode, name], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            Ok(ids)
        })
    }

    /// Updates a re-extracted derived file.
    pub fn update_derived_file(&self, file_id: i64, u: &DerivedUpdate) -> Result<()> {
        let split = |x: Option<(i64, u32)>| (x.map(|v| v.0), x.map(|v| v.1));
        let (mtime, mtime_ns) = split(u.times[0]);
        let (atime, atime_ns) = split(u.times[1]);
        let (ctime, ctime_ns) = split(u.times[2]);
        let (crtime, crtime_ns) = split(u.times[3]);
        self.write(|tx| {
            tx.execute(
                "UPDATE files SET size = ?2, mtime = ?3, mtime_ns = ?4, atime = ?5, atime_ns = ?6, ctime = ?7, ctime_ns = ?8, \
                 crtime = ?9, crtime_ns = ?10, local_path = ?11, mime = CASE WHEN ?12 THEN NULL ELSE mime END, \
                 md5 = NULL, sha1 = NULL, sha256 = NULL WHERE id = ?1",
                params![
                    file_id,
                    i64::try_from(u.size).unwrap_or(i64::MAX),
                    mtime,
                    mtime_ns,
                    atime,
                    atime_ns,
                    ctime,
                    ctime_ns,
                    crtime,
                    crtime_ns,
                    u.local_path,
                    u.reset_mime
                ],
            )?;
            Ok(())
        })
    }
}

//! The postmortem case database.
//!
//! * SQLite in WAL mode: one writer connection plus a pool of read-only
//!   connections, so the UI keeps querying while ingest writes.
//! * Versioned migrations (`migrations/*.sql` + Rust seeding) recorded in
//!   `meta.schema_version`; newer-than-supported databases are refused.
//! * [`Bulk`] batches inserts into large transactions (data-source loading
//!   inserts hundreds of thousands of rows).
//! * Typed records and queries; enum columns are small integers.
//!
//! The SQL avoids SQLite-only features where practical so a PostgreSQL back
//! end (multi-user cases, M11) can share it.

pub mod catalog;
mod model;

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};
use thiserror::Error;

pub use model::*;

/// Current schema version written by this build.
pub const SCHEMA_VERSION: i64 = 1;
const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_initial.sql")];
/// Rows per transaction in bulk mode.
const BULK_COMMIT_EVERY: usize = 50_000;
const MAX_IDLE_READERS: usize = 8;

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("case database already exists: {0}")]
    Exists(PathBuf),
    #[error("case database schema v{found} is newer than this postmortem (v{supported})")]
    TooNew { found: i64, supported: i64 },
    #[error("invalid value in database: {0}")]
    Invalid(String),
}

pub type Result<T, E = DbError> = std::result::Result<T, E>;

fn configure(conn: &Connection, writer: bool) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -65_536)?; // 64 MiB
    conn.pragma_update(None, "mmap_size", 268_435_456)?;
    if writer {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    Ok(())
}

/// An open case database.
#[derive(Debug)]
pub struct CaseDb {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
}

impl CaseDb {
    /// Creates a new database (fails if `path` exists).
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            return Err(DbError::Exists(path));
        }
        Self::open_inner(path)
    }

    /// Opens an existing database, applying pending migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(path.as_ref().to_path_buf())
    }

    fn open_inner(path: PathBuf) -> Result<Self> {
        let mut conn = Connection::open(&path)?;
        configure(&conn, true)?;
        migrate(&mut conn)?;
        Ok(Self { path, writer: Mutex::new(conn), readers: Mutex::new(Vec::new()) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` on a pooled read-only connection.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = match self.readers.lock().pop() {
            Some(c) => c,
            None => {
                let c = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
                configure(&c, false)?;
                c
            }
        };
        let out = f(&conn);
        let mut pool = self.readers.lock();
        if pool.len() < MAX_IDLE_READERS {
            pool.push(conn);
        }
        out
    }

    /// Runs `f` inside one write transaction (committed on `Ok`).
    pub fn write<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.writer.lock();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Bulk-insert mode: large transactions committed every
    /// `BULK_COMMIT_EVERY` rows (and at the end).
    pub fn bulk<T>(&self, f: impl FnOnce(&mut Bulk<'_>) -> Result<T>) -> Result<T> {
        let conn = self.writer.lock();
        conn.execute_batch("BEGIN")?;
        let mut b = Bulk { conn: &conn, pending: 0 };
        match f(&mut b) {
            Ok(v) => {
                conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.read(|c| schema_version(c))
    }

    // ------------------------------------------------------------ meta

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
            Ok(())
        })
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.read(|c| Ok(c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?))
    }

    // ------------------------------------------------------ data sources

    pub fn data_sources(&self) -> Result<Vec<DataSourceRecord>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT id, name, kind, paths, size, sector_size, timezone, image_format, md5, sha1, sha256, acquisition, added_at FROM data_sources ORDER BY id")?;
            let rows = st.query_map([], DataSourceRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn file_systems(&self, ds_id: i64) -> Result<Vec<FileSystemRecord>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT id, ds_id, volume_id, kind, byte_offset, block_size, block_count, root_inode, label, serial FROM file_systems WHERE ds_id = ?1 ORDER BY id")?;
            let rows = st.query_map([ds_id], FileSystemRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn volumes(&self, ds_id: i64) -> Result<Vec<VolumeRecord>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT v.id, v.addr, v.kind, v.start_sector, v.sector_count, v.description, v.name, v.flags, vs.sector_size \
                 FROM volumes v JOIN volume_systems vs ON vs.id = v.vs_id WHERE vs.ds_id = ?1 ORDER BY v.addr",
            )?;
            let rows = st.query_map([ds_id], VolumeRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Removes a data source and everything derived from it.
    pub fn delete_data_source(&self, ds_id: i64) -> Result<()> {
        self.write(|tx| {
            // Explicit child-first deletes avoid deep cascade recursion.
            tx.execute("DELETE FROM attributes WHERE artifact_id IN (SELECT id FROM artifacts WHERE ds_id = ?1)", [ds_id])?;
            tx.execute("DELETE FROM artifact_tags WHERE artifact_id IN (SELECT id FROM artifacts WHERE ds_id = ?1)", [ds_id])?;
            tx.execute("DELETE FROM artifacts WHERE ds_id = ?1", [ds_id])?;
            tx.execute("DELETE FROM file_tags WHERE file_id IN (SELECT id FROM files WHERE ds_id = ?1)", [ds_id])?;
            tx.execute("UPDATE files SET parent_id = NULL, derived_from = NULL WHERE ds_id = ?1", [ds_id])?;
            tx.execute("DELETE FROM files WHERE ds_id = ?1", [ds_id])?;
            tx.execute("DELETE FROM data_sources WHERE id = ?1", [ds_id])?;
            Ok(())
        })
    }

    // ------------------------------------------------------------- files

    pub fn file(&self, id: i64) -> Result<Option<FileRecord>> {
        self.read(|c| Ok(c.query_row(&format!("SELECT {FILE_COLS} FROM files WHERE id = ?1"), [id], FileRecord::from_row).optional()?))
    }

    /// Children of a directory, directories first then by name.
    pub fn children(&self, parent_id: i64) -> Result<Vec<FileRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!("SELECT {FILE_COLS} FROM files WHERE parent_id = ?1 ORDER BY kind <> 2, name COLLATE NOCASE"))?;
            let rows = st.query_map([parent_id], FileRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Root directory record of a filesystem.
    pub fn fs_root(&self, fs_id: i64) -> Result<Option<FileRecord>> {
        self.read(|c| {
            Ok(c.query_row(&format!("SELECT {FILE_COLS} FROM files WHERE fs_id = ?1 AND parent_id IS NULL AND origin = 0 ORDER BY id LIMIT 1"), [fs_id], FileRecord::from_row).optional()?)
        })
    }

    /// Files matching `q`, ordered by id, with keyset paging (`after_id`).
    pub fn query_files(&self, q: &FileQuery, after_id: i64, limit: usize) -> Result<Vec<FileRecord>> {
        let (clause, args) = q.where_clause();
        self.read(|c| {
            let sql = format!("SELECT {FILE_COLS} FROM files WHERE id > ?1 AND {clause} ORDER BY id LIMIT {limit}");
            let mut st = c.prepare_cached(&sql)?;
            let mut all: Vec<rusqlite::types::Value> = vec![after_id.into()];
            all.extend(args);
            let rows = st.query_map(rusqlite::params_from_iter(all), FileRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn count_files(&self, q: &FileQuery) -> Result<u64> {
        let (clause, args) = q.where_clause();
        self.read(|c| {
            let n: i64 = c.query_row(&format!("SELECT COUNT(*) FROM files WHERE {clause}"), rusqlite::params_from_iter(args), |r| r.get(0))?;
            Ok(n.max(0) as u64)
        })
    }

    /// File counts per extension (lower case), for the File Types view.
    pub fn extension_counts(&self, ds_id: Option<i64>) -> Result<Vec<(String, u64)>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT extension, COUNT(*) FROM files WHERE kind = 1 AND (?1 IS NULL OR ds_id = ?1) GROUP BY extension ORDER BY extension")?;
            let rows = st.query_map([ds_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Records content hashes and/or MIME type (ingest results).
    pub fn set_file_analysis(&self, id: i64, update: &FileAnalysis) -> Result<()> {
        self.write(|tx| update.apply(tx, id))
    }

    // --------------------------------------------------------- artifacts

    /// Inserts an artifact (or analysis result) with its attributes.
    pub fn add_artifact(&self, a: &NewArtifact) -> Result<i64> {
        self.write(|tx| insert_artifact(tx, a))
    }

    /// `(type id, count)` for every artifact type present.
    pub fn artifact_counts(&self, ds_id: Option<i64>) -> Result<Vec<(i64, u64)>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT type_id, COUNT(*) FROM artifacts WHERE review_status <> 2 AND (?1 IS NULL OR ds_id = ?1) GROUP BY type_id ORDER BY type_id")?;
            let rows = st.query_map([ds_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?.max(0) as u64)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn artifacts_of_type(&self, type_id: i64, ds_id: Option<i64>, after_id: i64, limit: usize) -> Result<Vec<ArtifactRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {ARTIFACT_COLS} FROM artifacts WHERE type_id = ?1 AND (?2 IS NULL OR ds_id = ?2) AND id > ?3 AND review_status <> 2 ORDER BY id LIMIT {limit}"
            ))?;
            let rows = st.query_map(params![type_id, ds_id, after_id], ArtifactRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn artifacts_for_file(&self, file_id: i64) -> Result<Vec<ArtifactRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!("SELECT {ARTIFACT_COLS} FROM artifacts WHERE file_id = ?1 ORDER BY id"))?;
            let rows = st.query_map([file_id], ArtifactRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn attributes(&self, artifact_id: i64) -> Result<Vec<AttributeRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT a.type_id, t.name, t.display_name, t.value_type, a.source, a.value_text, a.value_int, a.value_double, a.value_blob \
                 FROM attributes a JOIN attribute_types t ON t.id = a.type_id WHERE a.artifact_id = ?1 ORDER BY a.rowid",
            )?;
            let rows = st.query_map([artifact_id], AttributeRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Highest analysis-result significance per file (the "S" column).
    pub fn file_score(&self, file_id: i64) -> Result<Significance> {
        self.read(|c| {
            let s: Option<i64> = c.query_row("SELECT MAX(significance) FROM artifacts WHERE file_id = ?1 AND significance IS NOT NULL AND review_status <> 2", [file_id], |r| r.get(0))?;
            Ok(s.and_then(Significance::from_code).unwrap_or(Significance::Unknown))
        })
    }

    /// Registers (or finds) a custom artifact type.
    pub fn artifact_type(&self, name: &str, display: &str, category: catalog::Category) -> Result<i64> {
        self.write(|tx| {
            if let Some(id) = tx.query_row("SELECT id FROM artifact_types WHERE name = ?1", [name], |r| r.get(0)).optional()? {
                return Ok(id);
            }
            tx.execute("INSERT INTO artifact_types (id, name, display_name, category, custom) VALUES ((SELECT MAX(id) + 1 FROM artifact_types), ?1, ?2, ?3, 1)", params![name, display, category as i64])?;
            Ok(tx.last_insert_rowid())
        })
    }

    /// Registers (or finds) a custom attribute type.
    pub fn attribute_type(&self, name: &str, display: &str, value_type: catalog::ValueType) -> Result<i64> {
        self.write(|tx| {
            if let Some(id) = tx.query_row("SELECT id FROM attribute_types WHERE name = ?1", [name], |r| r.get(0)).optional()? {
                return Ok(id);
            }
            tx.execute("INSERT INTO attribute_types (name, display_name, value_type, custom) VALUES (?1, ?2, ?3, 1)", params![name, display, value_type as i64])?;
            Ok(tx.last_insert_rowid())
        })
    }

    pub fn artifact_type_names(&self) -> Result<Vec<(i64, String, String, i64)>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT id, name, display_name, category FROM artifact_types ORDER BY id")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    // -------------------------------------------------------------- tags

    pub fn tag_names(&self) -> Result<Vec<TagName>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT id, name, description, color, notable FROM tag_names ORDER BY name COLLATE NOCASE")?;
            let rows = st.query_map([], TagName::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn add_tag_name(&self, name: &str, description: &str, notable: bool) -> Result<i64> {
        self.write(|tx| {
            tx.execute("INSERT INTO tag_names (name, description, notable) VALUES (?1, ?2, ?3)", params![name, description, notable])?;
            Ok(tx.last_insert_rowid())
        })
    }

    pub fn tag_file(&self, file_id: i64, tag_name_id: i64, comment: &str, examiner: &str, now: i64) -> Result<i64> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO file_tags (file_id, tag_name_id, comment, examiner, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![file_id, tag_name_id, comment, examiner, now],
            )?;
            // A notable tag marks the file notable (Autopsy behaviour).
            tx.execute("UPDATE files SET known = 2 WHERE id = ?1 AND (SELECT notable FROM tag_names WHERE id = ?2) = 1", params![file_id, tag_name_id])?;
            Ok(tx.last_insert_rowid())
        })
    }

    pub fn untag_file(&self, file_tag_id: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM file_tags WHERE id = ?1", [file_tag_id])?;
            Ok(())
        })
    }

    pub fn file_tags(&self, file_id: i64) -> Result<Vec<FileTag>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT ft.id, ft.file_id, ft.tag_name_id, tn.name, ft.comment, ft.examiner, ft.created_at FROM file_tags ft JOIN tag_names tn ON tn.id = ft.tag_name_id WHERE ft.file_id = ?1 ORDER BY ft.id",
            )?;
            let rows = st.query_map([file_id], FileTag::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn files_with_tag(&self, tag_name_id: i64) -> Result<Vec<(FileTag, FileRecord)>> {
        let tags: Vec<FileTag> = self.read(|c| {
            let mut st = c.prepare(
                "SELECT ft.id, ft.file_id, ft.tag_name_id, tn.name, ft.comment, ft.examiner, ft.created_at FROM file_tags ft JOIN tag_names tn ON tn.id = ft.tag_name_id WHERE ft.tag_name_id = ?1 ORDER BY ft.id",
            )?;
            let rows = st.query_map([tag_name_id], FileTag::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        let mut out = Vec::with_capacity(tags.len());
        for t in tags {
            if let Some(f) = self.file(t.file_id)? {
                out.push((t, f));
            }
        }
        Ok(out)
    }

    /// `(tag name id, count)` of tagged files.
    pub fn tag_counts(&self) -> Result<Vec<(i64, u64)>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT tag_name_id, COUNT(*) FROM file_tags GROUP BY tag_name_id")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?.max(0) as u64)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    // ------------------------------------------------------- ingest jobs

    pub fn start_ingest_job(&self, ds_id: i64, settings_json: &str, now: i64, modules: &[(&str, &str)]) -> Result<i64> {
        self.write(|tx| {
            tx.execute("INSERT INTO ingest_jobs (ds_id, started_at, status, settings) VALUES (?1, ?2, 0, ?3)", params![ds_id, now, settings_json])?;
            let id = tx.last_insert_rowid();
            for (m, v) in modules {
                tx.execute("INSERT INTO ingest_job_modules (job_id, module, version) VALUES (?1, ?2, ?3)", params![id, m, v])?;
            }
            Ok(id)
        })
    }

    pub fn finish_ingest_job(&self, job_id: i64, cancelled: bool, now: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute("UPDATE ingest_jobs SET ended_at = ?2, status = ?3 WHERE id = ?1", params![job_id, now, if cancelled { 1 } else { 2 }])?;
            Ok(())
        })
    }
}

fn schema_version(c: &Connection) -> Result<i64> {
    let exists: bool = c.query_row("SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'meta'", [], |r| r.get(0))?;
    if !exists {
        return Ok(0);
    }
    let v: Option<String> = c.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0)).optional()?;
    v.map_or(Ok(0), |s| s.parse().map_err(|_| DbError::Invalid(format!("schema_version {s:?}"))))
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current = schema_version(conn)?;
    if current > SCHEMA_VERSION {
        return Err(DbError::TooNew { found: current, supported: SCHEMA_VERSION });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = i as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        if version == 1 {
            seed_catalog(&tx)?;
        }
        tx.execute("INSERT INTO meta(key, value) VALUES ('schema_version', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [version.to_string()])?;
        tx.commit()?;
        tracing::info!(version, "case database migrated");
    }
    Ok(())
}

fn seed_catalog(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let mut st = tx.prepare("INSERT INTO artifact_types (id, name, display_name, category) VALUES (?1, ?2, ?3, ?4)")?;
    for (id, name, display, cat) in catalog::ARTIFACT_TYPES {
        st.execute(params![id, name, display, *cat as i64])?;
    }
    let mut st = tx.prepare("INSERT INTO attribute_types (id, name, display_name, value_type) VALUES (?1, ?2, ?3, ?4)")?;
    for (i, (name, display, vt)) in catalog::ATTRIBUTE_TYPES.iter().enumerate() {
        st.execute(params![i as i64 + 1, name, display, *vt as i64])?;
    }
    Ok(())
}

fn insert_artifact(tx: &Connection, a: &NewArtifact) -> Result<i64> {
    let (sig, prio) = a.score.map_or((None, None), |(s, p)| (Some(s as i64), Some(p as i64)));
    tx.prepare_cached(
        "INSERT INTO artifacts (type_id, ds_id, file_id, source_artifact_id, significance, priority, conclusion, configuration, justification, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?
    .execute(params![a.type_id, a.ds_id, a.file_id, a.source_artifact_id, sig, prio, a.conclusion, a.configuration, a.justification, a.created_at])?;
    let id = tx.last_insert_rowid();
    let mut st = tx.prepare_cached(
        "INSERT INTO attributes (artifact_id, type_id, source, value_text, value_int, value_double, value_blob) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for (type_id, value, source) in &a.attributes {
        let (t, i, d, b) = value.columns();
        st.execute(params![id, type_id, source, t, i, d, b])?;
    }
    Ok(id)
}

/// Batched writer used while loading data sources and during ingest.
#[derive(Debug)]
pub struct Bulk<'c> {
    conn: &'c Connection,
    pending: usize,
}

impl Bulk<'_> {
    fn tick(&mut self) -> Result<()> {
        self.pending += 1;
        if self.pending >= BULK_COMMIT_EVERY {
            self.conn.execute_batch("COMMIT; BEGIN")?;
            self.pending = 0;
        }
        Ok(())
    }

    pub fn add_data_source(&mut self, d: &NewDataSource) -> Result<i64> {
        let paths = serde_json::to_string(&d.paths).map_err(|e| DbError::Invalid(e.to_string()))?;
        let acq = serde_json::to_string(&d.acquisition).map_err(|e| DbError::Invalid(e.to_string()))?;
        self.conn.execute(
            "INSERT INTO data_sources (name, kind, paths, size, sector_size, timezone, image_format, md5, sha1, sha256, acquisition, added_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![d.name, d.kind as i64, paths, d.size as i64, d.sector_size, d.timezone, d.image_format, d.md5, d.sha1, d.sha256, acq, d.added_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn add_volume_system(&mut self, ds_id: i64, kind: i64, sector_size: u32, disk_guid: Option<&str>) -> Result<i64> {
        self.conn.execute("INSERT INTO volume_systems (ds_id, kind, sector_size, disk_guid) VALUES (?1, ?2, ?3, ?4)", params![ds_id, kind, sector_size, disk_guid])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn add_volume(&mut self, vs_id: i64, v: &NewVolume) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO volumes (vs_id, addr, kind, start_sector, sector_count, description, name, flags) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![vs_id, v.addr, v.kind, v.start_sector as i64, v.sector_count as i64, v.description, v.name, v.flags as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn add_file_system(&mut self, f: &NewFileSystem) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO file_systems (ds_id, volume_id, kind, byte_offset, block_size, block_count, root_inode, label, serial) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![f.ds_id, f.volume_id, f.kind, f.byte_offset as i64, f.block_size, f.block_count as i64, f.root_inode as i64, f.label, f.serial],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn add_file(&mut self, f: &NewFile) -> Result<i64> {
        let mut st = self.conn.prepare_cached(INSERT_FILE)?;
        let t = |x: Option<(i64, u32)>| (x.map(|v| v.0), x.map(|v| v.1));
        let (m, m_ns) = t(f.times[0]);
        let (a, a_ns) = t(f.times[1]);
        let (c, c_ns) = t(f.times[2]);
        let (b, b_ns) = t(f.times[3]);
        st.execute(params![
            f.ds_id,
            f.fs_id,
            f.parent_id,
            f.origin as i64,
            f.inode.map(|i| i as i64),
            f.stream,
            f.name,
            f.parent_path,
            f.kind as i64,
            f.name_alloc,
            f.meta_alloc,
            f.meta_flags,
            f.size as i64,
            m,
            m_ns,
            a,
            a_ns,
            c,
            c_ns,
            b,
            b_ns,
            f.mode,
            f.uid,
            f.gid,
            f.seq,
            f.attributes,
            extension_of(&f.name),
            f.derived_from,
            f.local_path,
        ])?;
        let id = self.conn.last_insert_rowid();
        self.tick()?;
        Ok(id)
    }

    pub fn add_artifact(&mut self, a: &NewArtifact) -> Result<i64> {
        let id = insert_artifact(self.conn, a)?;
        self.tick()?;
        Ok(id)
    }

    pub fn set_file_analysis(&mut self, id: i64, update: &FileAnalysis) -> Result<()> {
        update.apply(self.conn, id)?;
        self.tick()
    }
}

/// Lower-case extension of a file name (`""` if none), as stored.
pub fn extension_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains(':') && ext.len() <= 16 => ext.to_lowercase(),
        _ => String::new(),
    }
}

const INSERT_FILE: &str = "INSERT INTO files (ds_id, fs_id, parent_id, origin, inode, stream, name, parent_path, kind, name_alloc, meta_alloc, meta_flags, size, \
    mtime, mtime_ns, atime, atime_ns, ctime, ctime_ns, crtime, crtime_ns, mode, uid, gid, seq, attributes, extension, derived_from, local_path) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)";

pub(crate) const FILE_COLS: &str = "id, ds_id, fs_id, parent_id, origin, inode, stream, name, parent_path, kind, name_alloc, meta_alloc, meta_flags, size, \
    mtime, mtime_ns, atime, atime_ns, ctime, ctime_ns, crtime, crtime_ns, mode, uid, gid, seq, attributes, extension, mime, md5, sha1, sha256, known, derived_from, local_path";

pub(crate) const ARTIFACT_COLS: &str = "id, type_id, ds_id, file_id, source_artifact_id, significance, priority, conclusion, configuration, justification, review_status, created_at";

pub(crate) fn opt_u64(r: &Row<'_>, i: usize) -> rusqlite::Result<Option<u64>> {
    Ok(r.get::<_, Option<i64>>(i)?.map(|v| v.max(0) as u64))
}

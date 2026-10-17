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

mod artifact_events;
mod browsers;
pub mod catalog;
pub mod communications;
mod derived;
pub mod events;
pub mod file_search;
pub mod gallery;
mod hosts;
pub mod integrity;
pub mod keyword_hits;
pub mod mcp;
mod model;
pub mod os_accounts;
mod reports;
pub mod summary;
pub use reports::Report;
mod scores;
mod search;
mod tags;
mod windows_artifacts;
pub use browsers::{ArtifactRow, NameFilter};

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};
use thiserror::Error;

pub use derived::{DerivedUpdate, FileIds};
pub use hosts::{Host, Person, default_host_name};
pub use model::*;
pub use scores::{ObjKind, Score};
pub use search::KeywordHitKey;
pub use tags::{ArtifactTag, NewTagName, PROJECT_VIC, TagSet};

/// Current schema version written by this build.
pub const SCHEMA_VERSION: i64 = 3;
/// Oldest schema readable without an upgrade: v3 only adds indexes.
pub const MIN_READABLE_SCHEMA: i64 = 2;
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_schema_v2.sql"),
    include_str!("../migrations/0003_timeline_indexes.sql"),
];
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
    file_ids: FileIds,
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
        let file_ids = FileIds::load(&conn)?;
        Ok(Self {
            path,
            writer: Mutex::new(conn),
            readers: Mutex::new(Vec::new()),
            file_ids,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` on a pooled read-only connection.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = if let Some(c) = self.readers.lock().pop() {
            c
        } else {
            let c = Connection::open_with_flags(
                &self.path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            configure(&c, false)?;
            c
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
        let mut b = Bulk {
            conn: &conn,
            pending: 0,
            ids: &self.file_ids,
        };
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

    /// Drops the secondary indexes on `files` before a large load (index
    /// maintenance dominates insert cost at scale). Pair with
    /// [`Self::rebuild_file_indexes`].
    pub fn defer_file_indexes(&self) -> Result<()> {
        let conn = self.writer.lock();
        for (name, _) in FILE_INDEXES {
            conn.execute_batch(&format!("DROP INDEX IF EXISTS {name}"))?;
        }
        Ok(())
    }

    /// (Re)creates the secondary indexes on `files` and refreshes planner
    /// statistics.
    pub fn rebuild_file_indexes(&self) -> Result<()> {
        let conn = self.writer.lock();
        for (name, ddl) in FILE_INDEXES {
            conn.execute_batch(&format!("CREATE INDEX IF NOT EXISTS {name} {ddl}"))?;
        }
        conn.execute_batch("ANALYZE files")?;
        Ok(())
    }

    /// `MAX(id) + 1` of the rows written so far. Use [`Self::file_ids`]
    /// to allocate ids.
    pub fn next_file_id(&self) -> Result<i64> {
        self.read(|c| {
            Ok(
                c.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM files", [], |r| {
                    r.get(0)
                })?,
            )
        })
    }

    /// Layout JSON of an unallocated-space or carved file.
    pub fn file_layout(&self, id: i64) -> Result<Option<String>> {
        self.read(|c| {
            Ok(
                c.query_row("SELECT layout FROM files WHERE id = ?1", [id], |r| r.get(0))
                    .optional()?
                    .flatten(),
            )
        })
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.read(schema_version)
    }

    // ------------------------------------------------------------ meta

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
            Ok(())
        })
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.read(|c| {
            Ok(
                c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                    .optional()?,
            )
        })
    }

    // ------------------------------------------------------ data sources

    pub fn data_sources(&self) -> Result<Vec<DataSourceRecord>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT id, name, kind, paths, size, sector_size, timezone, image_format, md5, sha1, sha256, acquisition, added_at FROM data_sources ORDER BY id")?;
            let rows = st.query_map([], DataSourceRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn data_source(&self, id: i64) -> Result<Option<DataSourceRecord>> {
        self.read(|c| {
            Ok(c
                .query_row("SELECT id, name, kind, paths, size, sector_size, timezone, image_format, md5, sha1, sha256, acquisition, added_at FROM data_sources WHERE id = ?1", [id], DataSourceRecord::from_row)
                .optional()?)
        })
    }

    pub fn file_system(&self, id: i64) -> Result<Option<FileSystemRecord>> {
        self.read(|c| {
            Ok(c
                .query_row("SELECT id, ds_id, volume_id, kind, byte_offset, block_size, block_count, root_inode, label, serial FROM file_systems WHERE id = ?1", [id], FileSystemRecord::from_row)
                .optional()?)
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
    ///
    /// Foreign-key enforcement is suspended and dependents are deleted
    /// explicitly, child-first: with enforcement on, every deleted file row
    /// would probe each referencing column, which is quadratic while the
    /// secondary indexes are deferred (e.g. when a load is cancelled).
    pub fn delete_data_source(&self, ds_id: i64) -> Result<()> {
        let mut conn = self.writer.lock();
        conn.pragma_update(None, "foreign_keys", "OFF")?;
        let out = (|| -> Result<()> {
            let tx = conn.transaction()?;
            for sql in [
                "DELETE FROM attributes WHERE artifact_id IN (SELECT id FROM artifacts WHERE ds_id = ?1)",
                "DELETE FROM artifact_tags WHERE artifact_id IN (SELECT id FROM artifacts WHERE ds_id = ?1)",
                "DELETE FROM artifacts WHERE ds_id = ?1",
                "DELETE FROM file_tags WHERE file_id IN (SELECT id FROM files WHERE ds_id = ?1)",
                "DELETE FROM files WHERE ds_id = ?1",
                "DELETE FROM file_systems WHERE ds_id = ?1",
                "DELETE FROM volumes WHERE vs_id IN (SELECT id FROM volume_systems WHERE ds_id = ?1)",
                "DELETE FROM volume_systems WHERE ds_id = ?1",
                "DELETE FROM ingest_job_modules WHERE job_id IN (SELECT id FROM ingest_jobs WHERE ds_id = ?1)",
                "DELETE FROM ingest_jobs WHERE ds_id = ?1",
                "DELETE FROM data_sources WHERE id = ?1",
            ] {
                tx.execute(sql, [ds_id])?;
            }
            tx.commit()?;
            Ok(())
        })();
        conn.pragma_update(None, "foreign_keys", "ON")?;
        out
    }

    /// Updates the image paths of a data source (evidence moved).
    pub fn set_data_source_paths(&self, ds_id: i64, paths: &[String]) -> Result<()> {
        let json = serde_json::to_string(paths).map_err(|e| DbError::Invalid(e.to_string()))?;
        self.write(|tx| {
            tx.execute(
                "UPDATE data_sources SET paths = ?2 WHERE id = ?1",
                params![ds_id, json],
            )?;
            Ok(())
        })
    }

    // ------------------------------------------------------------- files

    pub fn file(&self, id: i64) -> Result<Option<FileRecord>> {
        self.read(|c| {
            Ok(c.query_row(
                &format!("SELECT {FILE_COLS} FROM files WHERE id = ?1"),
                [id],
                FileRecord::from_row,
            )
            .optional()?)
        })
    }

    /// Children of a directory, directories first then by name.
    /// The file-system file with metadata address `inode` (and alternate
    /// stream `stream`) in file system `fs_id` (indexed lookup).
    pub fn file_by_inode(
        &self,
        fs_id: i64,
        inode: u64,
        stream: Option<u32>,
    ) -> Result<Option<FileRecord>> {
        let Ok(inode) = i64::try_from(inode) else {
            return Ok(None);
        };
        self.read(|c| {
            Ok(c.query_row(
                &format!(
                    "SELECT {FILE_COLS} FROM files WHERE fs_id = ?1 AND inode = ?2 AND stream IS ?3 AND origin = 0 ORDER BY id LIMIT 1"
                ),
                params![fs_id, inode, stream],
                FileRecord::from_row,
            )
            .optional()?)
        })
    }

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
    pub fn query_files(
        &self,
        q: &FileQuery,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<FileRecord>> {
        let (clause, args) = q.where_clause(1);
        self.read(|c| {
            let sql = format!(
                "SELECT {FILE_COLS} FROM files WHERE id > ?1 AND {clause} ORDER BY id LIMIT {limit}"
            );
            let mut st = c.prepare_cached(&sql)?;
            let mut all: Vec<rusqlite::types::Value> = vec![after_id.into()];
            all.extend(args);
            let rows = st
                .query_map(rusqlite::params_from_iter(all), FileRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Files of a data source in one ingest priority class, by id, with
    /// keyset paging (Autopsy's root-directory priorities; see
    /// [`IngestPriority`]).
    pub fn ingest_files(
        &self,
        ds_id: i64,
        class: IngestPriority,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<FileRecord>> {
        self.read(|c| {
            let sql = format!(
                "SELECT {FILE_COLS} FROM files WHERE id > ?1 AND ds_id = ?2 AND ({INGEST_CLASS}) = ?3 ORDER BY id LIMIT {limit}"
            );
            let mut st = c.prepare_cached(&sql)?;
            let rows = st
                .query_map(params![after_id, ds_id, class as i64], FileRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn count_files(&self, q: &FileQuery) -> Result<u64> {
        let (clause, args) = q.where_clause(0);
        self.read(|c| {
            let n: i64 = c.query_row(
                &format!("SELECT COUNT(*) FROM files WHERE {clause}"),
                rusqlite::params_from_iter(args),
                |r| r.get(0),
            )?;
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

    /// Number of files per detected MIME type.
    pub fn mime_counts(&self, ds_id: Option<i64>) -> Result<Vec<(String, u64)>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT mime, COUNT(*) FROM files WHERE mime IS NOT NULL AND (?1 IS NULL OR ds_id = ?1) GROUP BY mime ORDER BY mime")?;
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

    pub fn artifacts_of_type(
        &self,
        type_id: i64,
        ds_id: Option<i64>,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<ArtifactRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {ARTIFACT_COLS} FROM artifacts WHERE type_id = ?1 AND (?2 IS NULL OR ds_id = ?2) AND id > ?3 AND review_status <> 2 ORDER BY id LIMIT {limit}"
            ))?;
            let rows = st.query_map(params![type_id, ds_id, after_id], ArtifactRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn artifact(&self, id: i64) -> Result<Option<ArtifactRecord>> {
        self.read(|c| {
            Ok(c.prepare_cached(&format!(
                "SELECT {ARTIFACT_COLS} FROM artifacts WHERE id = ?1"
            ))?
            .query_row([id], ArtifactRecord::from_row)
            .optional()?)
        })
    }

    pub fn artifacts_for_file(&self, file_id: i64) -> Result<Vec<ArtifactRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {ARTIFACT_COLS} FROM artifacts WHERE file_id = ?1 ORDER BY id"
            ))?;
            let rows = st
                .query_map([file_id], ArtifactRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
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

    /// A file's aggregate significance (the "S" column).
    pub fn file_score(&self, file_id: i64) -> Result<Significance> {
        Ok(self.score(ObjKind::File, file_id)?.significance)
    }

    /// Registers (or finds) a custom artifact type.
    pub fn artifact_type(
        &self,
        name: &str,
        display: &str,
        category: catalog::Category,
    ) -> Result<i64> {
        self.write(|tx| {
            if let Some(id) = tx.query_row("SELECT id FROM artifact_types WHERE name = ?1", [name], |r| r.get(0)).optional()? {
                return Ok(id);
            }
            tx.execute("INSERT INTO artifact_types (id, name, display_name, category, custom) VALUES ((SELECT MAX(id) + 1 FROM artifact_types), ?1, ?2, ?3, 1)", params![name, display, category as i64])?;
            Ok(tx.last_insert_rowid())
        })
    }

    /// Registers (or finds) a custom attribute type.
    pub fn attribute_type(
        &self,
        name: &str,
        display: &str,
        value_type: catalog::ValueType,
    ) -> Result<i64> {
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
            let mut st = c.prepare(
                "SELECT id, name, display_name, category FROM artifact_types ORDER BY id",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// The id of an artifact type by name, standard or custom.
    pub fn artifact_type_id_named(&self, name: &str) -> Result<Option<i64>> {
        if let Some(id) = catalog::artifact_type_id(name) {
            return Ok(Some(id));
        }
        self.read(|c| {
            Ok(
                c.prepare_cached("SELECT id FROM artifact_types WHERE name = ?1")?
                    .query_row([name], |r| r.get(0))
                    .optional()?,
            )
        })
    }

    /// The id of an attribute type by name, standard or custom.
    pub fn attribute_type_id_named(&self, name: &str) -> Result<Option<i64>> {
        if let Some(id) = catalog::attribute_type_id(name) {
            return Ok(Some(id));
        }
        self.read(|c| {
            Ok(
                c.prepare_cached("SELECT id FROM attribute_types WHERE name = ?1")?
                    .query_row([name], |r| r.get(0))
                    .optional()?,
            )
        })
    }

    // ------------------------------------------------------- ingest jobs

    pub fn start_ingest_job(
        &self,
        ds_id: i64,
        settings_json: &str,
        now: i64,
        modules: &[(&str, &str)],
    ) -> Result<i64> {
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
            tx.execute(
                "UPDATE ingest_jobs SET ended_at = ?2, status = ?3 WHERE id = ?1",
                params![job_id, now, if cancelled { 1 } else { 2 }],
            )?;
            Ok(())
        })
    }
}

fn schema_version(c: &Connection) -> Result<i64> {
    let exists: bool = c.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    v.map_or(Ok(0), |s| {
        s.parse()
            .map_err(|_| DbError::Invalid(format!("schema_version {s:?}")))
    })
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current = schema_version(conn)?;
    if current > SCHEMA_VERSION {
        return Err(DbError::TooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
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
    let mut st = tx.prepare(
        "INSERT INTO artifact_types (id, name, display_name, category) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (id, name, display, cat) in catalog::ARTIFACT_TYPES {
        st.execute(params![id, name, display, *cat as i64])?;
    }
    let mut st = tx.prepare(
        "INSERT INTO attribute_types (id, name, display_name, value_type) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (i, (name, display, vt)) in catalog::ATTRIBUTE_TYPES.iter().enumerate() {
        st.execute(params![i as i64 + 1, name, display, *vt as i64])?;
    }
    Ok(())
}

fn insert_artifact(tx: &Connection, a: &NewArtifact) -> Result<i64> {
    let (sig, prio) = a
        .score
        .map_or((None, None), |(s, p)| (Some(s as i64), Some(p as i64)));
    tx.prepare_cached(
        "INSERT INTO artifacts (type_id, ds_id, file_id, source_artifact_id, significance, priority, conclusion, configuration, justification, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?
    .execute(params![a.type_id, a.ds_id, a.file_id, a.source_artifact_id, sig, prio, a.conclusion, a.configuration, a.justification, a.created_at])?;
    let id = tx.last_insert_rowid();
    if let (Some((significance, priority)), Some((kind, obj))) = (
        a.score,
        scores::scored_object(a.file_id, a.source_artifact_id),
    ) {
        scores::raise(
            tx,
            kind,
            obj,
            Some(a.ds_id),
            Score {
                significance,
                priority,
            },
        )?;
    }
    let mut st = tx.prepare_cached(
        "INSERT INTO attributes (artifact_id, type_id, source, value_text, value_int, value_double, value_blob) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for (type_id, value, source) in &a.attributes {
        let (text, int, double, blob) = value.columns();
        st.execute(params![id, type_id, source, text, int, double, blob])?;
    }
    artifact_events::add_artifact_events(tx, id, a)?;
    Ok(id)
}

fn artifact_exists(c: &Connection, a: &NewArtifact, key: &[i64]) -> Result<bool> {
    let Some(file_id) = a.file_id else {
        return Ok(false);
    };
    let wanted: Vec<(i64, &Value)> = a
        .attributes
        .iter()
        .filter(|(t, _, _)| key.contains(t))
        .map(|(t, v, _)| (*t, v))
        .collect();
    let mut st =
        c.prepare_cached("SELECT id FROM artifacts WHERE file_id = ?1 AND type_id = ?2")?;
    let ids = st
        .query_map(params![file_id, a.type_id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut attrs = c.prepare_cached(
        "SELECT COUNT(*) FROM attributes WHERE artifact_id = ?1 AND type_id = ?2 \
         AND value_text IS ?3 AND value_int IS ?4 AND value_double IS ?5 AND value_blob IS ?6",
    )?;
    for id in ids {
        let mut all = true;
        for (t, v) in &wanted {
            let (text, int, double, blob) = v.columns();
            let n: i64 = attrs.query_row(params![id, t, text, int, double, blob], |r| r.get(0))?;
            if n == 0 {
                all = false;
                break;
            }
        }
        if all {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Batched writer used while loading data sources and during ingest.
#[derive(Debug)]
pub struct Bulk<'c> {
    conn: &'c Connection,
    pending: usize,
    ids: &'c FileIds,
}

impl Bulk<'_> {
    /// [`CaseDb::account_instance`] in this bulk transaction.
    pub fn account_instance(
        &mut self,
        inst: communications::AccountInstanceSpec<'_>,
    ) -> Result<(communications::Account, i64)> {
        communications::account_instance_tx(self.conn, inst)
    }

    /// [`CaseDb::add_relationships`] in this bulk transaction.
    pub fn add_relationships(
        &mut self,
        participants: &[i64],
        source_artifact_id: i64,
        kind: communications::RelationshipKind,
        date_time: Option<i64>,
        ds_id: i64,
    ) -> Result<()> {
        communications::relationships_tx(
            self.conn,
            participants,
            source_artifact_id,
            kind,
            date_time,
            ds_id,
        )
    }

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
        let acq =
            serde_json::to_string(&d.acquisition).map_err(|e| DbError::Invalid(e.to_string()))?;
        self.conn.execute(
            "INSERT INTO data_sources (name, kind, paths, size, sector_size, timezone, image_format, md5, sha1, sha256, acquisition, added_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![d.name, d.kind as i64, paths, d.size as i64, d.sector_size, d.timezone, d.image_format, d.md5, d.sha1, d.sha256, acq, d.added_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Puts a data source on the host `name`, creating the host if needed.
    pub fn assign_host(&mut self, ds_id: i64, name: &str) -> Result<Host> {
        let host = hosts::host_named(self.conn, name)?;
        self.conn.execute(
            "UPDATE data_sources SET host_id = ?2 WHERE id = ?1",
            params![ds_id, host.id],
        )?;
        Ok(host)
    }

    pub fn add_volume_system(
        &mut self,
        ds_id: i64,
        kind: i64,
        sector_size: u32,
        disk_guid: Option<&str>,
    ) -> Result<i64> {
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

    /// Inserts a file row. Without an explicit id it takes the next one
    /// from the shared allocator ([`CaseDb::file_ids`]); never let SQLite
    /// choose, or it could take an id another writer has reserved.
    pub fn add_file(&mut self, f: &NewFile) -> Result<i64> {
        let id = match f.id {
            Some(id) => {
                self.ids.observe(id);
                id
            }
            None => self.ids.next(),
        };
        let mut st = self.conn.prepare_cached(INSERT_FILE)?;
        let split = |x: Option<(i64, u32)>| (x.map(|v| v.0), x.map(|v| v.1));
        let (mtime, mtime_ns) = split(f.times[0]);
        let (atime, atime_ns) = split(f.times[1]);
        let (ctime, ctime_ns) = split(f.times[2]);
        let (crtime, crtime_ns) = split(f.times[3]);
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
            mtime,
            mtime_ns,
            atime,
            atime_ns,
            ctime,
            ctime_ns,
            crtime,
            crtime_ns,
            f.mode,
            f.uid,
            f.gid,
            f.seq,
            f.attributes,
            extension_of(&f.name),
            f.derived_from,
            f.local_path,
            f.layout,
            id,
        ])?;
        self.tick()?;
        Ok(id)
    }

    pub fn add_artifact(&mut self, a: &NewArtifact) -> Result<i64> {
        let id = insert_artifact(self.conn, a)?;
        self.tick()?;
        Ok(id)
    }

    /// Adds `a` unless the same file already has an artifact of the same
    /// type whose attributes include `a`'s attributes of the `key` types
    /// (Autopsy's `artifactExists`). Returns the new id, or `None`.
    pub fn add_artifact_unless_exists(
        &mut self,
        a: &NewArtifact,
        key: &[i64],
    ) -> Result<Option<i64>> {
        if artifact_exists(self.conn, a, key)? {
            return Ok(None);
        }
        self.add_artifact(a).map(Some)
    }

    pub fn set_file_analysis(&mut self, id: i64, update: &FileAnalysis) -> Result<()> {
        update.apply(self.conn, id)?;
        self.tick()
    }
}

/// Lower-case extension of a file name (`""` if none), as stored.
pub fn extension_of(name: &str) -> String {
    match name.rsplit_once('.') {
        // Alternate data streams ("file.exe:Zone.Identifier") have no extension.
        Some((stem, ext))
            if !stem.is_empty() && !stem.contains(':') && !ext.contains(':') && ext.len() <= 16 =>
        {
            ext.to_lowercase()
        }
        _ => String::new(),
    }
}

/// Secondary indexes on `files` (must match the migrations).
const FILE_INDEXES: &[(&str, &str)] = &[
    ("files_parent", "ON files(parent_id, name)"),
    ("files_ds_kind", "ON files(ds_id, kind)"),
    ("files_ext", "ON files(extension, ds_id) WHERE kind = 1"),
    ("files_mime", "ON files(mime, ds_id) WHERE mime IS NOT NULL"),
    ("files_md5", "ON files(md5) WHERE md5 IS NOT NULL"),
    ("files_sha1", "ON files(sha1) WHERE sha1 IS NOT NULL"),
    ("files_sha256", "ON files(sha256) WHERE sha256 IS NOT NULL"),
    ("files_mtime", "ON files(mtime)"),
    ("files_atime", "ON files(atime) WHERE atime IS NOT NULL"),
    ("files_ctime", "ON files(ctime) WHERE ctime IS NOT NULL"),
    ("files_crtime", "ON files(crtime) WHERE crtime IS NOT NULL"),
    ("files_size", "ON files(size) WHERE kind = 1"),
    (
        "files_deleted",
        "ON files(ds_id) WHERE name_alloc = 0 OR meta_alloc = 0",
    ),
    ("files_inode", "ON files(fs_id, inode)"),
    (
        "files_derived",
        "ON files(derived_from) WHERE derived_from IS NOT NULL",
    ),
];

const INSERT_FILE: &str = "INSERT INTO files (ds_id, fs_id, parent_id, origin, inode, stream, name, parent_path, kind, name_alloc, meta_alloc, meta_flags, size, \
    mtime, mtime_ns, atime, atime_ns, ctime, ctime_ns, crtime, crtime_ns, mode, uid, gid, seq, attributes, extension, derived_from, local_path, layout, id) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31)";

pub(crate) const FILE_COLS: &str = "id, ds_id, fs_id, parent_id, origin, inode, stream, name, parent_path, kind, name_alloc, meta_alloc, meta_flags, size, \
    mtime, mtime_ns, atime, atime_ns, ctime, ctime_ns, crtime, crtime_ns, mode, uid, gid, seq, attributes, extension, mime, md5, sha1, sha256, known, derived_from, local_path";

/// SQL computing a file's [`IngestPriority`] from the first component of
/// its path (`LIKE` is ASCII case-insensitive, as Autopsy's patterns are).
const INGEST_CLASS: &str = "CASE WHEN origin <> 0 THEN 3 ELSE (SELECT CASE \
      WHEN t LIKE 'pagefile%' OR t LIKE 'hiberfil%' THEN 3 \
      WHEN t LIKE 'users%' OR t LIKE 'documents and settings%' OR t LIKE 'home%' OR t LIKE 'programdata%' THEN 0 \
      WHEN t LIKE '$orphanfiles%' OR t LIKE 'windows%' THEN 2 \
      ELSE 1 END FROM (SELECT CASE WHEN parent_path = '/' THEN name \
      ELSE substr(parent_path, 2, instr(substr(parent_path, 2), '/') - 1) END AS t)) END";

pub(crate) const ARTIFACT_COLS: &str = "id, type_id, ds_id, file_id, source_artifact_id, significance, priority, conclusion, configuration, justification, review_status, created_at";

pub(crate) fn opt_u64(r: &Row<'_>, i: usize) -> rusqlite::Result<Option<u64>> {
    Ok(r.get::<_, Option<i64>>(i)?.map(|v| v.max(0) as u64))
}

//! Read helpers for the MCP server (`pm-mcp`): read-only opening and
//! keyset-paged, filterable queries over files, artifacts, tags, events,
//! communications, ingest jobs and reports. No schema changes.
//!
//! Every list query takes a keyset cursor and a limit and is ordered by an
//! indexed key where one exists (ADR-014). Filters on columns without an
//! index (`LIKE '%…%'`, `atime`/`ctime`/`crtime`) still stop after `limit`
//! matching rows.

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter};

use crate::communications::Account;
use crate::events::EventRecord;
use crate::tags::ArtifactTag;
use crate::{
    ARTIFACT_COLS, ArtifactRecord, CaseDb, DbError, FILE_COLS, FileIds, FileKindCode, FileRecord,
    FileTag, Known, Result, SCHEMA_VERSION, Significance,
};

/// Escapes `%`, `_` and `\` for `LIKE … ESCAPE '\'`.
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `%text%` for a case-insensitive (ASCII) substring `LIKE`.
fn contains(s: &str) -> SqlValue {
    SqlValue::Text(format!("%{}%", like_escape(s)))
}

/// A WHERE clause under construction with positional (`?`) arguments.
#[derive(Debug, Default)]
struct Where {
    parts: Vec<String>,
    args: Vec<SqlValue>,
}

impl Where {
    fn push(&mut self, cond: impl Into<String>, args: impl IntoIterator<Item = SqlValue>) {
        self.parts.push(cond.into());
        self.args.extend(args);
    }

    fn sql(&self) -> String {
        if self.parts.is_empty() {
            "1=1".into()
        } else {
            self.parts.join(" AND ")
        }
    }
}

fn int(v: i64) -> SqlValue {
    SqlValue::Integer(v)
}

fn clamp_u64(v: u64) -> SqlValue {
    SqlValue::Integer(i64::try_from(v).unwrap_or(i64::MAX))
}

// ------------------------------------------------------------------ opening

impl CaseDb {
    /// Opens an existing case database without write access (no
    /// migrations, no WAL change): a second process such as the MCP server
    /// can read it while the GUI writes. Databases that need a migration
    /// are refused; open them once in postmortem first.
    pub fn open_read_only(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        crate::configure(&conn, false)?;
        let version = crate::schema_version(&conn)?;
        if version > SCHEMA_VERSION {
            return Err(DbError::TooNew {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version < SCHEMA_VERSION {
            return Err(DbError::Invalid(format!(
                "case database schema v{version} needs an upgrade to v{SCHEMA_VERSION}; open the case in postmortem once"
            )));
        }
        let file_ids = FileIds::load(&conn)?;
        Ok(Self {
            path,
            writer: parking_lot::Mutex::new(conn),
            readers: parking_lot::Mutex::new(Vec::new()),
            file_ids,
        })
    }

    /// Whether the database was opened with [`Self::open_read_only`].
    pub fn is_read_only(&self) -> bool {
        self.writer
            .lock()
            .is_readonly(rusqlite::MAIN_DB)
            .unwrap_or(true)
    }
}

// -------------------------------------------------------------------- files

/// A file time column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeColumn {
    Modified,
    Accessed,
    Changed,
    Created,
}

impl TimeColumn {
    pub const ALL: [Self; 4] = [Self::Modified, Self::Accessed, Self::Changed, Self::Created];

    /// Column name in `files`.
    pub fn column(self) -> &'static str {
        match self {
            Self::Modified => "mtime",
            Self::Accessed => "atime",
            Self::Changed => "ctime",
            Self::Created => "crtime",
        }
    }

    /// Index into [`FileRecord::times`].
    pub fn index(self) -> usize {
        match self {
            Self::Modified => 0,
            Self::Accessed => 1,
            Self::Changed => 2,
            Self::Created => 3,
        }
    }

    /// TSK timeline event type id of the column (File Modified, …).
    pub fn event_type(self) -> i64 {
        match self {
            Self::Modified => crate::events::types::FILE_MODIFIED,
            Self::Accessed => crate::events::types::FILE_ACCESSED,
            Self::Changed => crate::events::types::FILE_CHANGED,
            Self::Created => crate::events::types::FILE_CREATED,
        }
    }
}

/// Score buckets of Autopsy's Score tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreClass {
    /// Notable ("Bad Items").
    Bad,
    /// Likely notable ("Suspicious Items").
    Suspicious,
    /// Either.
    Any,
}

impl ScoreClass {
    fn sql(self) -> &'static str {
        match self {
            Self::Bad => "s.significance = 4",
            Self::Suspicious => "s.significance = 2",
            Self::Any => "s.significance IN (2, 4)",
        }
    }
}

/// Conditions for [`CaseDb::mcp_files`] (all `AND`ed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFilter {
    pub ds_id: Option<i64>,
    /// Restrict to these ids.
    pub ids: Option<Vec<i64>>,
    /// Name substring (ASCII case-insensitive).
    pub name_contains: Option<String>,
    /// Exact name (case-insensitive).
    pub name_equals: Option<String>,
    /// Lower-case extensions without the dot.
    pub extensions: Vec<String>,
    /// Exact MIME type, or a prefix when it ends with `*`.
    pub mime: Option<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// `(column, from, to)` inclusive ranges in Unix seconds.
    pub times: Vec<(TimeColumn, Option<i64>, Option<i64>)>,
    /// Parent path substring (ASCII case-insensitive).
    pub path_contains: Option<String>,
    /// Files anywhere below this folder path (`/a/b/`).
    pub under_path: Option<String>,
    /// Direct children of this file id.
    pub parent_id: Option<i64>,
    pub kind: Option<FileKindCode>,
    /// Name allocated (`true`) or unallocated (`false`).
    pub allocated: Option<bool>,
    /// Deleted (name or metadata unallocated).
    pub deleted: Option<bool>,
    pub known: Option<Known>,
    pub md5: Option<Vec<u8>>,
    pub sha1: Option<Vec<u8>>,
    pub sha256: Option<Vec<u8>>,
    /// Tagged with this tag name.
    pub tag_name_id: Option<i64>,
    pub score: Option<ScoreClass>,
    /// Has a hash-set hit.
    pub hash_set_hit: bool,
    /// Has an extension-mismatch result.
    pub extension_mismatch: bool,
    /// Leave out this file id.
    pub exclude_id: Option<i64>,
    /// Every stream of one metadata entry: `(file system id, inode)`.
    pub inode: Option<(i64, u64)>,
}

impl FileFilter {
    fn build(&self, w: &mut Where) {
        if let Some(ds) = self.ds_id {
            w.push("ds_id = ?", [int(ds)]);
        }
        if let Some(ids) = &self.ids {
            if ids.is_empty() {
                w.push("0", []);
            } else {
                let ph = vec!["?"; ids.len()].join(", ");
                w.push(format!("id IN ({ph})"), ids.iter().map(|&i| int(i)));
            }
        }
        if let Some(n) = &self.name_contains {
            w.push("name LIKE ? ESCAPE '\\'", [contains(n)]);
        }
        if let Some(n) = &self.name_equals {
            w.push("name = ? COLLATE NOCASE", [SqlValue::Text(n.clone())]);
        }
        if !self.extensions.is_empty() {
            let ph = vec!["?"; self.extensions.len()].join(", ");
            w.push(
                format!("extension IN ({ph})"),
                self.extensions
                    .iter()
                    .map(|e| SqlValue::Text(e.trim_start_matches('.').to_lowercase())),
            );
        }
        if let Some(m) = &self.mime {
            if let Some(prefix) = m.strip_suffix('*') {
                w.push(
                    "mime LIKE ? ESCAPE '\\'",
                    [SqlValue::Text(format!("{}%", like_escape(prefix)))],
                );
            } else {
                w.push("mime = ?", [SqlValue::Text(m.clone())]);
            }
        }
        if let Some(v) = self.min_size {
            w.push("size >= ?", [clamp_u64(v)]);
        }
        if let Some(v) = self.max_size {
            w.push("size <= ?", [clamp_u64(v)]);
        }
        for (col, from, to) in &self.times {
            let c = col.column();
            if let Some(f) = from {
                w.push(format!("{c} >= ?"), [int(*f)]);
            }
            if let Some(t) = to {
                w.push(format!("{c} <= ?"), [int(*t)]);
            }
        }
        if let Some(p) = &self.path_contains {
            w.push("parent_path LIKE ? ESCAPE '\\'", [contains(p)]);
        }
        if let Some(p) = &self.under_path {
            let mut p = p.clone();
            if !p.ends_with('/') {
                p.push('/');
            }
            w.push(
                "substr(parent_path, 1, ?) = ?",
                [int(p.len() as i64), SqlValue::Text(p)],
            );
        }
        if let Some(p) = self.parent_id {
            w.push("parent_id = ?", [int(p)]);
        }
        if let Some(k) = self.kind {
            w.push("kind = ?", [int(k as i64)]);
        }
        if let Some(a) = self.allocated {
            w.push("name_alloc = ?", [int(i64::from(a))]);
        }
        match self.deleted {
            Some(true) => w.push("(name_alloc = 0 OR meta_alloc = 0)", []),
            Some(false) => w.push("(name_alloc = 1 AND meta_alloc = 1)", []),
            None => {}
        }
        if let Some(k) = self.known {
            w.push("known = ?", [int(k as i64)]);
        }
        for (col, h) in [
            ("md5", &self.md5),
            ("sha1", &self.sha1),
            ("sha256", &self.sha256),
        ] {
            if let Some(h) = h {
                w.push(format!("{col} = ?"), [SqlValue::Blob(h.clone())]);
            }
        }
        if let Some(t) = self.tag_name_id {
            w.push(
                "EXISTS (SELECT 1 FROM file_tags ft WHERE ft.file_id = files.id AND ft.tag_name_id = ?)",
                [int(t)],
            );
        }
        if let Some(s) = self.score {
            w.push(
                format!(
                    "EXISTS (SELECT 1 FROM scores s WHERE s.obj_kind = 0 AND s.obj_id = files.id AND {})",
                    s.sql()
                ),
                [],
            );
        }
        if self.hash_set_hit {
            w.push(
                "EXISTS (SELECT 1 FROM artifacts a WHERE a.file_id = files.id AND a.type_id = 10)",
                [],
            );
        }
        if self.extension_mismatch {
            w.push(
                "EXISTS (SELECT 1 FROM artifacts a JOIN artifact_types t ON t.id = a.type_id \
                 WHERE a.file_id = files.id AND t.name = 'TSK_EXT_MISMATCH_DETECTED')",
                [],
            );
        }
        if let Some(x) = self.exclude_id {
            w.push("id <> ?", [int(x)]);
        }
        if let Some((fs, inode)) = self.inode {
            w.push("fs_id = ? AND inode = ?", [int(fs), clamp_u64(inode)]);
        }
    }
}

/// Sort orders of [`CaseDb::mcp_files`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileOrder {
    #[default]
    Id,
    Size,
    Name,
    Modified,
    Created,
}

impl FileOrder {
    fn expr(self) -> Option<&'static str> {
        match self {
            Self::Id => None,
            Self::Size => Some("size"),
            Self::Name => Some("name COLLATE NOCASE"),
            Self::Modified => Some("IFNULL(mtime, -9223372036854775808)"),
            Self::Created => Some("IFNULL(crtime, -9223372036854775808)"),
        }
    }

    /// The sort key of a row (for the next cursor).
    pub fn key(self, f: &FileRecord) -> SortKey {
        match self {
            Self::Id => SortKey::None,
            Self::Size => SortKey::Int(i64::try_from(f.size).unwrap_or(i64::MAX)),
            Self::Name => SortKey::Text(f.name.clone()),
            Self::Modified => SortKey::Int(f.times[0].map_or(i64::MIN, |t| t.0)),
            Self::Created => SortKey::Int(f.times[3].map_or(i64::MIN, |t| t.0)),
        }
    }
}

/// The sort-column value of a keyset cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortKey {
    None,
    Int(i64),
    Text(String),
}

impl SortKey {
    fn value(&self) -> SqlValue {
        match self {
            Self::None => SqlValue::Null,
            Self::Int(i) => int(*i),
            Self::Text(s) => SqlValue::Text(s.clone()),
        }
    }
}

/// Position after the last row of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCursor {
    pub key: SortKey,
    pub id: i64,
}

impl CaseDb {
    /// One keyset page of files matching `f`, sorted by `order` (then id).
    pub fn mcp_files(
        &self,
        f: &FileFilter,
        order: FileOrder,
        descending: bool,
        after: Option<&FileCursor>,
        limit: usize,
    ) -> Result<Vec<FileRecord>> {
        let mut w = Where::default();
        f.build(&mut w);
        let (cmp, dir) = if descending {
            ("<", "DESC")
        } else {
            (">", "ASC")
        };
        let order_sql = match order.expr() {
            Some(e) => format!("{e} {dir}, id {dir}"),
            None => format!("id {dir}"),
        };
        if let Some(c) = after {
            match order.expr() {
                Some(e) => w.push(
                    format!("({e}, id) {cmp} (?, ?)"),
                    [c.key.value(), int(c.id)],
                ),
                None => w.push(format!("id {cmp} ?"), [int(c.id)]),
            }
        }
        let sql = format!(
            "SELECT {FILE_COLS} FROM files WHERE {} ORDER BY {order_sql} LIMIT {}",
            w.sql(),
            limit.min(100_000)
        );
        self.read(|c| {
            let mut st = c.prepare_cached(&sql)?;
            let rows = st
                .query_map(params_from_iter(w.args.iter()), FileRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Several files by id (missing ids are skipped), in the given order.
    pub fn files_by_ids(&self, ids: &[i64]) -> Result<Vec<FileRecord>> {
        self.read(|c| {
            let mut st =
                c.prepare_cached(&format!("SELECT {FILE_COLS} FROM files WHERE id = ?1"))?;
            let mut out = Vec::with_capacity(ids.len());
            for &id in ids {
                if let Some(f) = st.query_row([id], FileRecord::from_row).optional()? {
                    out.push(f);
                }
            }
            Ok(out)
        })
    }

    /// The owner of a file: `(owner uid / SID, OS account id)`.
    pub fn mcp_file_owner(&self, file_id: i64) -> Result<(Option<String>, Option<i64>)> {
        self.read(|c| {
            Ok(
                c.prepare_cached("SELECT owner_uid, os_account_id FROM files WHERE id = ?1")?
                    .query_row([file_id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?
                    .unwrap_or((None, None)),
            )
        })
    }

    /// Number of file rows (all data sources, or one).
    pub fn mcp_file_count(&self, ds_id: Option<i64>) -> Result<u64> {
        self.read(|c| {
            let n: i64 = c
                .prepare_cached("SELECT COUNT(*) FROM files WHERE (?1 IS NULL OR ds_id = ?1)")?
                .query_row([ds_id], |r| r.get(0))?;
            Ok(n.max(0) as u64)
        })
    }

    /// Number of artifact rows (all data sources, or one).
    pub fn mcp_artifact_count(&self, ds_id: Option<i64>) -> Result<u64> {
        self.read(|c| {
            let n: i64 = c
                .prepare_cached("SELECT COUNT(*) FROM artifacts WHERE (?1 IS NULL OR ds_id = ?1)")?
                .query_row([ds_id], |r| r.get(0))?;
            Ok(n.max(0) as u64)
        })
    }
}

// ---------------------------------------------------------------- artifacts

/// A condition on an artifact's attributes.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrPredicate {
    /// A text attribute (of the type, or any type) contains the text
    /// (ASCII case-insensitive).
    TextContains { type_id: Option<i64>, text: String },
    /// A text attribute equals the text (case-insensitive).
    TextEquals { type_id: i64, text: String },
    /// An integer / date-time attribute equals the value.
    IntEquals { type_id: i64, value: i64 },
    /// An integer / date-time attribute of one of the types lies in the
    /// inclusive range.
    IntRange {
        type_ids: Vec<i64>,
        from: Option<i64>,
        to: Option<i64>,
    },
    /// A floating-point attribute lies in the inclusive range.
    DoubleRange {
        type_id: i64,
        from: Option<f64>,
        to: Option<f64>,
    },
    /// The artifact has an attribute of the type.
    Has { type_id: i64 },
}

impl AttrPredicate {
    fn build(&self, w: &mut Where) {
        const E: &str = "EXISTS (SELECT 1 FROM attributes x WHERE x.artifact_id = artifacts.id AND";
        match self {
            Self::TextContains { type_id, text } => match type_id {
                Some(t) => w.push(
                    format!("{E} x.type_id = ? AND x.value_text LIKE ? ESCAPE '\\')"),
                    [int(*t), contains(text)],
                ),
                None => w.push(
                    format!("{E} x.value_text LIKE ? ESCAPE '\\')"),
                    [contains(text)],
                ),
            },
            Self::TextEquals { type_id, text } => w.push(
                format!("{E} x.type_id = ? AND x.value_text = ? COLLATE NOCASE)"),
                [int(*type_id), SqlValue::Text(text.clone())],
            ),
            Self::IntEquals { type_id, value } => w.push(
                format!("{E} x.type_id = ? AND x.value_int = ?)"),
                [int(*type_id), int(*value)],
            ),
            Self::IntRange { type_ids, from, to } => {
                if type_ids.is_empty() {
                    w.push("0", []);
                    return;
                }
                let ph = vec!["?"; type_ids.len()].join(", ");
                let mut cond = format!("{E} x.type_id IN ({ph})");
                let mut args: Vec<SqlValue> = type_ids.iter().map(|&t| int(t)).collect();
                if let Some(f) = from {
                    cond.push_str(" AND x.value_int >= ?");
                    args.push(int(*f));
                }
                if let Some(t) = to {
                    cond.push_str(" AND x.value_int <= ?");
                    args.push(int(*t));
                }
                cond.push(')');
                w.push(cond, args);
            }
            Self::DoubleRange { type_id, from, to } => {
                let mut cond = format!("{E} x.type_id = ?");
                let mut args = vec![int(*type_id)];
                if let Some(f) = from {
                    cond.push_str(" AND x.value_double >= ?");
                    args.push(SqlValue::Real(*f));
                }
                if let Some(t) = to {
                    cond.push_str(" AND x.value_double <= ?");
                    args.push(SqlValue::Real(*t));
                }
                cond.push(')');
                w.push(cond, args);
            }
            Self::Has { type_id } => w.push(format!("{E} x.type_id = ?)"), [int(*type_id)]),
        }
    }
}

/// Conditions for [`CaseDb::mcp_artifacts`] (all `AND`ed).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtifactFilter {
    /// Data artifacts (`Some(false)`) or analysis results (`Some(true)`).
    pub analysis: Option<bool>,
    /// Restrict to these types (empty: any).
    pub type_ids: Vec<i64>,
    pub ds_id: Option<i64>,
    pub file_id: Option<i64>,
    pub source_artifact_id: Option<i64>,
    pub attrs: Vec<AttrPredicate>,
    /// Analysis results with one of these significances.
    pub significance: Vec<Significance>,
    /// Include results the examiner rejected.
    pub include_rejected: bool,
    /// Restrict to these ids.
    pub ids: Option<Vec<i64>>,
}

impl ArtifactFilter {
    fn build(&self, w: &mut Where) {
        if let Some(a) = self.analysis {
            w.push(
                "type_id IN (SELECT id FROM artifact_types WHERE category = ?)",
                [int(i64::from(a))],
            );
        }
        if !self.type_ids.is_empty() {
            let ph = vec!["?"; self.type_ids.len()].join(", ");
            w.push(
                format!("type_id IN ({ph})"),
                self.type_ids.iter().map(|&t| int(t)),
            );
        }
        if let Some(d) = self.ds_id {
            w.push("ds_id = ?", [int(d)]);
        }
        if let Some(f) = self.file_id {
            w.push("file_id = ?", [int(f)]);
        }
        if let Some(s) = self.source_artifact_id {
            w.push("source_artifact_id = ?", [int(s)]);
        }
        for p in &self.attrs {
            p.build(w);
        }
        if !self.significance.is_empty() {
            let ph = vec!["?"; self.significance.len()].join(", ");
            w.push(
                format!("significance IN ({ph})"),
                self.significance.iter().map(|&s| int(s as i64)),
            );
        }
        if !self.include_rejected {
            w.push("review_status <> 2", []);
        }
        if let Some(ids) = &self.ids {
            if ids.is_empty() {
                w.push("0", []);
            } else {
                let ph = vec!["?"; ids.len()].join(", ");
                w.push(format!("id IN ({ph})"), ids.iter().map(|&i| int(i)));
            }
        }
    }
}

/// An artifact type row with its use count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactTypeCount {
    pub id: i64,
    pub name: String,
    pub display_name: String,
    /// `true` for analysis results.
    pub analysis: bool,
    pub count: u64,
}

impl CaseDb {
    /// One keyset page of artifacts matching `f`, by id.
    pub fn mcp_artifacts(
        &self,
        f: &ArtifactFilter,
        after_id: Option<i64>,
        descending: bool,
        limit: usize,
    ) -> Result<Vec<ArtifactRecord>> {
        let mut w = Where::default();
        f.build(&mut w);
        let (cmp, dir) = if descending {
            ("<", "DESC")
        } else {
            (">", "ASC")
        };
        if let Some(a) = after_id {
            w.push(format!("id {cmp} ?"), [int(a)]);
        }
        let sql = format!(
            "SELECT {ARTIFACT_COLS} FROM artifacts WHERE {} ORDER BY id {dir} LIMIT {}",
            w.sql(),
            limit.min(100_000)
        );
        self.read(|c| {
            let mut st = c.prepare_cached(&sql)?;
            let rows = st
                .query_map(params_from_iter(w.args.iter()), ArtifactRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Artifact types in use with their counts (optionally for one data
    /// source), by id.
    pub fn mcp_artifact_type_counts(&self, ds_id: Option<i64>) -> Result<Vec<ArtifactTypeCount>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT t.id, t.name, t.display_name, t.category, n.cnt FROM artifact_types t \
                 JOIN (SELECT type_id, COUNT(*) AS cnt FROM artifacts WHERE review_status <> 2 \
                       AND (?1 IS NULL OR ds_id = ?1) GROUP BY type_id) n ON n.type_id = t.id ORDER BY t.id",
            )?;
            let rows = st
                .query_map([ds_id], |r| {
                    Ok(ArtifactTypeCount {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        display_name: r.get(2)?,
                        analysis: r.get::<_, i64>(3)? == 1,
                        count: r.get::<_, i64>(4)?.max(0) as u64,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// An artifact type by name: `(id, display name, analysis result?)`.
    pub fn artifact_type_by_name(&self, name: &str) -> Result<Option<(i64, String, bool)>> {
        self.read(|c| {
            Ok(c.prepare_cached(
                "SELECT id, display_name, category FROM artifact_types WHERE name = ?1 COLLATE NOCASE",
            )?
            .query_row([name], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? == 1)))
            .optional()?)
        })
    }

    /// An artifact type by id: `(name, display name, analysis result?)`.
    pub fn artifact_type_by_id(&self, id: i64) -> Result<Option<(String, String, bool)>> {
        self.read(|c| {
            Ok(c.prepare_cached(
                "SELECT name, display_name, category FROM artifact_types WHERE id = ?1",
            )?
            .query_row([id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? == 1))
            })
            .optional()?)
        })
    }

    /// An attribute type by name: `(id, value type code)`.
    pub fn attribute_type_by_name(&self, name: &str) -> Result<Option<(i64, i64)>> {
        self.read(|c| {
            Ok(c.prepare_cached(
                "SELECT id, value_type FROM attribute_types WHERE name = ?1 COLLATE NOCASE",
            )?
            .query_row([name], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?)
        })
    }

    /// The file attributes (`file_attributes`) of a file:
    /// `(type name, value)`.
    pub fn file_attribute_values(&self, file_id: i64) -> Result<Vec<(String, crate::Value)>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT t.name, t.value_type, a.value_text, a.value_int, a.value_double, a.value_blob \
                 FROM file_attributes a JOIN attribute_types t ON t.id = a.type_id WHERE a.file_id = ?1 ORDER BY a.id",
            )?;
            let rows = st
                .query_map([file_id], |r| {
                    Ok((
                        r.get(0)?,
                        crate::Value::from_columns(r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

// --------------------------------------------------------------------- tags

/// A file or result tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagRow {
    File(FileTag),
    Artifact(ArtifactTag),
}

impl CaseDb {
    /// One keyset page of tags (file tags first, then result tags, each by
    /// id), optionally of one tag name. `after` is `(0 file | 1 result,
    /// tag id)`.
    pub fn mcp_tags(
        &self,
        tag_name_id: Option<i64>,
        after: Option<(u8, i64)>,
        limit: usize,
    ) -> Result<Vec<TagRow>> {
        let (kind, id) = after.unwrap_or((0, 0));
        let lim = limit.min(100_000) as i64;
        self.read(|c| {
            let mut out = Vec::new();
            if kind == 0 {
                let mut st = c.prepare_cached(
                    "SELECT ft.id, ft.file_id, ft.tag_name_id, tn.name, ft.comment, ft.examiner, ft.created_at \
                     FROM file_tags ft JOIN tag_names tn ON tn.id = ft.tag_name_id \
                     WHERE (?1 IS NULL OR ft.tag_name_id = ?1) AND ft.id > ?2 ORDER BY ft.id LIMIT ?3",
                )?;
                for t in st.query_map(params![tag_name_id, id, lim], FileTag::from_row)? {
                    out.push(TagRow::File(t?));
                }
            }
            let left = lim - out.len() as i64;
            if left > 0 {
                let after_art = if kind == 1 { id } else { 0 };
                let mut st = c.prepare_cached(
                    "SELECT at.id, at.artifact_id, at.tag_name_id, tn.name, at.comment, at.examiner, at.created_at \
                     FROM artifact_tags at JOIN tag_names tn ON tn.id = at.tag_name_id \
                     WHERE (?1 IS NULL OR at.tag_name_id = ?1) AND at.id > ?2 ORDER BY at.id LIMIT ?3",
                )?;
                for t in st.query_map(params![tag_name_id, after_art, left], |r| {
                    Ok(ArtifactTag {
                        id: r.get(0)?,
                        artifact_id: r.get(1)?,
                        tag_name_id: r.get(2)?,
                        tag_name: r.get(3)?,
                        comment: r.get(4)?,
                        examiner: r.get(5)?,
                        created_at: r.get(6)?,
                    })
                })? {
                    out.push(TagRow::Artifact(t?));
                }
            }
            Ok(out)
        })
    }
}

// ------------------------------------------------------------------- events

/// Where a timeline page continues. Rows are ordered by
/// `(time, source rank, id)`; a source query uses the mode that matches
/// its rank relative to the cursor's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKeyset {
    /// From the start.
    Start,
    /// Strictly after `(time, id)` (same source).
    After { time: i64, id: i64 },
    /// Strictly after `time` (sources ranked before the cursor's).
    AfterTime(i64),
    /// At or after `time` (sources ranked after the cursor's).
    FromTime(i64),
}

impl EventKeyset {
    fn build(self, w: &mut Where, time: &str, id: &str, descending: bool) {
        let (gt, ge) = if descending { ("<", "<=") } else { (">", ">=") };
        match self {
            Self::Start => {}
            Self::After { time: t, id: i } => {
                w.push(format!("({time}, {id}) {gt} (?, ?)"), [int(t), int(i)]);
            }
            Self::AfterTime(t) => w.push(format!("{time} {gt} ?"), [int(t)]),
            Self::FromTime(t) => w.push(format!("{time} {ge} ?"), [int(t)]),
        }
    }
}

/// Conditions shared by stored and file-system events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFilter {
    /// Inclusive start / exclusive end (Unix seconds).
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub ds_id: Option<i64>,
    /// Description (stored events) or path (file events) substring.
    pub text: Option<String>,
    pub tagged_only: bool,
    pub hash_hit_only: bool,
    pub file_id: Option<i64>,
    pub artifact_id: Option<i64>,
}

impl CaseDb {
    /// One keyset page of stored (artifact) events of the given types
    /// (empty: all), ordered by `(time, id)`.
    pub fn mcp_events(
        &self,
        type_ids: &[i64],
        f: &EventFilter,
        after: EventKeyset,
        descending: bool,
        limit: usize,
    ) -> Result<Vec<EventRecord>> {
        let mut w = Where::default();
        if let Some(s) = f.start {
            w.push("e.time >= ?", [int(s)]);
        }
        if let Some(e) = f.end {
            w.push("e.time < ?", [int(e)]);
        }
        if !type_ids.is_empty() {
            let ph = vec!["?"; type_ids.len()].join(", ");
            w.push(
                format!("e.type_id IN ({ph})"),
                type_ids.iter().map(|&t| int(t)),
            );
        }
        if let Some(d) = f.ds_id {
            w.push("d.ds_id = ?", [int(d)]);
        }
        if let Some(t) = &f.text {
            w.push("d.full_description LIKE ? ESCAPE '\\'", [contains(t)]);
        }
        if f.tagged_only {
            w.push("d.tagged = 1", []);
        }
        if f.hash_hit_only {
            w.push("d.hash_hit = 1", []);
        }
        if let Some(x) = f.file_id {
            w.push("d.file_id = ?", [int(x)]);
        }
        if let Some(x) = f.artifact_id {
            w.push("d.artifact_id = ?", [int(x)]);
        }
        after.build(&mut w, "e.time", "e.id", descending);
        let dir = if descending { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT e.id, e.type_id, e.time, d.full_description, d.med_description, d.short_description, d.ds_id, \
                    d.file_id, d.artifact_id, d.hash_hit, d.tagged \
             FROM events e JOIN event_descriptions d ON d.id = e.description_id \
             WHERE {} ORDER BY e.time {dir}, e.id {dir} LIMIT {}",
            w.sql(),
            limit.min(100_000)
        );
        self.read(|c| {
            let mut st = c.prepare_cached(&sql)?;
            let rows = st
                .query_map(params_from_iter(w.args.iter()), |r| {
                    Ok(EventRecord {
                        id: r.get(0)?,
                        type_id: r.get(1)?,
                        time: r.get(2)?,
                        full_description: r.get(3)?,
                        med_description: r.get(4)?,
                        short_description: r.get(5)?,
                        ds_id: r.get(6)?,
                        file_id: r.get(7)?,
                        artifact_id: r.get(8)?,
                        hash_hit: r.get(9)?,
                        tagged: r.get(10)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// One stored event by id.
    pub fn mcp_event(&self, id: i64) -> Result<Option<EventRecord>> {
        self.read(|c| {
            Ok(c.prepare_cached(
                "SELECT e.id, e.type_id, e.time, d.full_description, d.med_description, d.short_description, d.ds_id, \
                        d.file_id, d.artifact_id, d.hash_hit, d.tagged \
                 FROM events e JOIN event_descriptions d ON d.id = e.description_id WHERE e.id = ?1",
            )?
            .query_row([id], |r| {
                Ok(EventRecord {
                    id: r.get(0)?,
                    type_id: r.get(1)?,
                    time: r.get(2)?,
                    full_description: r.get(3)?,
                    med_description: r.get(4)?,
                    short_description: r.get(5)?,
                    ds_id: r.get(6)?,
                    file_id: r.get(7)?,
                    artifact_id: r.get(8)?,
                    hash_hit: r.get(9)?,
                    tagged: r.get(10)?,
                })
            })
            .optional()?)
        })
    }

    fn file_event_where(col: TimeColumn, f: &EventFilter, w: &mut Where) {
        let c = col.column();
        w.push(format!("{c} IS NOT NULL"), []);
        if let Some(s) = f.start {
            w.push(format!("{c} >= ?"), [int(s)]);
        }
        if let Some(e) = f.end {
            w.push(format!("{c} < ?"), [int(e)]);
        }
        if let Some(d) = f.ds_id {
            w.push("ds_id = ?", [int(d)]);
        }
        if let Some(t) = &f.text {
            w.push("(parent_path || name) LIKE ? ESCAPE '\\'", [contains(t)]);
        }
        if f.tagged_only {
            w.push(
                "EXISTS (SELECT 1 FROM file_tags ft WHERE ft.file_id = files.id)",
                [],
            );
        }
        if f.hash_hit_only {
            w.push(
                "EXISTS (SELECT 1 FROM artifacts a WHERE a.file_id = files.id AND a.type_id = 10)",
                [],
            );
        }
        if let Some(x) = f.file_id {
            w.push("id = ?", [int(x)]);
        }
        if f.artifact_id.is_some() {
            w.push("0", []);
        }
    }

    /// One keyset page of the virtual file-system events of one time
    /// column (ADR-018), ordered by `(time, file id)`.
    pub fn mcp_file_events(
        &self,
        col: TimeColumn,
        f: &EventFilter,
        after: EventKeyset,
        descending: bool,
        limit: usize,
    ) -> Result<Vec<FileRecord>> {
        let mut w = Where::default();
        Self::file_event_where(col, f, &mut w);
        let c = col.column();
        after.build(&mut w, c, "id", descending);
        let dir = if descending { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT {FILE_COLS} FROM files WHERE {} ORDER BY {c} {dir}, id {dir} LIMIT {}",
            w.sql(),
            limit.min(100_000)
        );
        self.read(|conn| {
            let mut st = conn.prepare_cached(&sql)?;
            let rows = st
                .query_map(params_from_iter(w.args.iter()), FileRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Stored-event counts per `(bucket, event type)` with
    /// `bucket = (time - origin) / width`.
    pub fn mcp_event_histogram(
        &self,
        f: &EventFilter,
        origin: i64,
        width: i64,
    ) -> Result<Vec<(i64, i64, u64)>> {
        let width = width.max(1);
        let mut w = Where::default();
        if let Some(s) = f.start {
            w.push("e.time >= ?", [int(s)]);
        }
        if let Some(e) = f.end {
            w.push("e.time < ?", [int(e)]);
        }
        if let Some(d) = f.ds_id {
            w.push("d.ds_id = ?", [int(d)]);
        }
        if let Some(t) = &f.text {
            w.push("d.full_description LIKE ? ESCAPE '\\'", [contains(t)]);
        }
        if f.tagged_only {
            w.push("d.tagged = 1", []);
        }
        if f.hash_hit_only {
            w.push("d.hash_hit = 1", []);
        }
        let sql = format!(
            "SELECT (e.time - {origin}) / {width} AS b, e.type_id, COUNT(*) FROM events e \
             JOIN event_descriptions d ON d.id = e.description_id WHERE {} GROUP BY b, e.type_id ORDER BY b, e.type_id",
            w.sql()
        );
        self.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map(params_from_iter(w.args.iter()), |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?.max(0) as u64))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// File-system event counts of one time column per bucket.
    pub fn mcp_file_event_histogram(
        &self,
        col: TimeColumn,
        f: &EventFilter,
        origin: i64,
        width: i64,
    ) -> Result<Vec<(i64, u64)>> {
        let width = width.max(1);
        let mut w = Where::default();
        Self::file_event_where(col, f, &mut w);
        let c = col.column();
        let sql = format!(
            "SELECT ({c} - {origin}) / {width} AS b, COUNT(*) FROM files WHERE {} GROUP BY b ORDER BY b",
            w.sql()
        );
        self.read(|conn| {
            let mut st = conn.prepare(&sql)?;
            let rows = st
                .query_map(params_from_iter(w.args.iter()), |r| {
                    Ok((r.get(0)?, r.get::<_, i64>(1)?.max(0) as u64))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// `(min, max)` time of stored events and file `mtime`s.
    pub fn mcp_time_span(&self) -> Result<Option<(i64, i64)>> {
        self.read(|c| {
            let ev: (Option<i64>, Option<i64>) =
                c.query_row("SELECT MIN(time), MAX(time) FROM events", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            let fs: (Option<i64>, Option<i64>) = c.query_row(
                "SELECT MIN(mtime), MAX(mtime) FROM files WHERE mtime > 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let lo = [ev.0, fs.0].into_iter().flatten().min();
            let hi = [ev.1, fs.1].into_iter().flatten().max();
            Ok(lo.zip(hi))
        })
    }
}

// ----------------------------------------------------------- communications

impl CaseDb {
    /// One keyset page of communication accounts (by id), optionally of
    /// one type, with their relationship counts.
    pub fn mcp_accounts(
        &self,
        type_id: Option<i64>,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<(Account, u64)>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT a.id, a.type_id, t.name, a.identifier, \
                   (SELECT COUNT(*) FROM account_relationships r WHERE r.account1_id = a.id) + \
                   (SELECT COUNT(*) FROM account_relationships r WHERE r.account2_id = a.id) \
                 FROM accounts a JOIN account_types t ON t.id = a.type_id \
                 WHERE (?1 IS NULL OR a.type_id = ?1) AND a.id > ?2 AND t.name <> 'DEVICE' ORDER BY a.id LIMIT ?3",
            )?;
            let rows = st
                .query_map(params![type_id, after_id, limit.min(100_000) as i64], |r| {
                    Ok((
                        Account {
                            id: r.get(0)?,
                            type_id: r.get(1)?,
                            type_name: r.get(2)?,
                            identifier: r.get(3)?,
                        },
                        r.get::<_, i64>(4)?.max(0) as u64,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// One keyset page of the relationships of `account_id` (with
    /// `other`, when given) of the given kinds (empty: all), ordered by
    /// `(date, artifact id)`: `(date, source artifact id, kind)`.
    pub fn mcp_relationships(
        &self,
        account_id: i64,
        other: Option<i64>,
        kinds: &[i64],
        after: Option<(i64, i64)>,
        limit: usize,
    ) -> Result<Vec<(Option<i64>, i64, i64)>> {
        let mut w = Where::default();
        w.push(
            "((r.account1_id = ? AND (? IS NULL OR r.account2_id = ?)) OR (r.account2_id = ? AND (? IS NULL OR r.account1_id = ?)))",
            [
                int(account_id),
                other.map_or(SqlValue::Null, int),
                other.map_or(SqlValue::Null, int),
                int(account_id),
                other.map_or(SqlValue::Null, int),
                other.map_or(SqlValue::Null, int),
            ],
        );
        if !kinds.is_empty() {
            let ph = vec!["?"; kinds.len()].join(", ");
            w.push(format!("r.kind IN ({ph})"), kinds.iter().map(|&k| int(k)));
        }
        if let Some((t, a)) = after {
            w.push(
                "(IFNULL(r.date_time, -1), r.source_artifact_id) > (?, ?)",
                [int(t), int(a)],
            );
        }
        let sql = format!(
            "SELECT DISTINCT r.date_time, r.source_artifact_id, r.kind FROM account_relationships r WHERE {} \
             ORDER BY IFNULL(r.date_time, -1), r.source_artifact_id LIMIT {}",
            w.sql(),
            limit.min(100_000)
        );
        self.read(|c| {
            let mut st = c.prepare_cached(&sql)?;
            let rows = st
                .query_map(params_from_iter(w.args.iter()), |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

// ------------------------------------------------------ ingest jobs, reports

/// An ingest job with its modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestJobRow {
    pub id: i64,
    pub ds_id: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    /// 0 started, 1 cancelled, 2 completed.
    pub status: i64,
    pub settings: String,
    /// `(module, version)`.
    pub modules: Vec<(String, String)>,
}

/// A generated report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    pub id: i64,
    pub path: String,
    pub module: String,
    pub display_name: String,
    pub created_at: i64,
}

/// A volume system with its volumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSystemRow {
    pub id: i64,
    /// 0 DOS, 1 GPT.
    pub kind: i64,
    pub sector_size: u32,
    pub volumes: Vec<(i64, crate::VolumeRecord)>,
}

impl CaseDb {
    /// Ingest jobs (optionally of one data source), newest first.
    pub fn mcp_ingest_jobs(&self, ds_id: Option<i64>, limit: usize) -> Result<Vec<IngestJobRow>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT id, ds_id, started_at, ended_at, status, settings FROM ingest_jobs \
                 WHERE (?1 IS NULL OR ds_id = ?1) ORDER BY id DESC LIMIT ?2",
            )?;
            let mut jobs = st
                .query_map(params![ds_id, limit.min(10_000) as i64], |r| {
                    Ok(IngestJobRow {
                        id: r.get(0)?,
                        ds_id: r.get(1)?,
                        started_at: r.get(2)?,
                        ended_at: r.get(3)?,
                        status: r.get(4)?,
                        settings: r.get(5)?,
                        modules: Vec::new(),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut m = c.prepare_cached(
                "SELECT module, version FROM ingest_job_modules WHERE job_id = ?1 ORDER BY rowid",
            )?;
            for j in &mut jobs {
                j.modules = m
                    .query_map([j.id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
            }
            Ok(jobs)
        })
    }

    /// Generated reports, by id.
    pub fn mcp_reports(&self) -> Result<Vec<ReportRow>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT id, path, module, display_name, created_at FROM reports ORDER BY id",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(ReportRow {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        module: r.get(2)?,
                        display_name: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Volume systems of a data source with their volumes (volume ids
    /// paired with the records).
    pub fn mcp_volume_systems(&self, ds_id: i64) -> Result<Vec<VolumeSystemRow>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT id, kind, sector_size FROM volume_systems WHERE ds_id = ?1 ORDER BY id",
            )?;
            let mut out = st
                .query_map([ds_id], |r| {
                    Ok(VolumeSystemRow {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        sector_size: r.get(2)?,
                        volumes: Vec::new(),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut v = c.prepare_cached(
                "SELECT v.id, v.addr, v.kind, v.start_sector, v.sector_count, v.description, v.name, v.flags, vs.sector_size \
                 FROM volumes v JOIN volume_systems vs ON vs.id = v.vs_id WHERE v.vs_id = ?1 ORDER BY v.addr",
            )?;
            for vs in &mut out {
                vs.volumes = v
                    .query_map([vs.id], |r| Ok((r.get(0)?, crate::VolumeRecord::from_row(r)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
            }
            Ok(out)
        })
    }

    /// File-system ids within a volume.
    pub fn mcp_file_systems_of_volume(&self, volume_id: i64) -> Result<Vec<i64>> {
        self.read(|c| {
            let mut st =
                c.prepare_cached("SELECT id FROM file_systems WHERE volume_id = ?1 ORDER BY id")?;
            let rows = st
                .query_map([volume_id], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DataSourceKind, NewArtifact, NewDataSource, NewFile, Origin, Value};

    fn db() -> (tempfile::TempDir, CaseDb, i64) {
        let dir = tempfile::tempdir().unwrap();
        let db = CaseDb::create(dir.path().join("case.db")).unwrap();
        let ds = db
            .bulk(|b| {
                let ds = b.add_data_source(&NewDataSource {
                    name: "img".into(),
                    kind: DataSourceKind::DiskImage,
                    paths: vec![],
                    size: 0,
                    sector_size: 512,
                    timezone: "UTC".into(),
                    image_format: None,
                    md5: None,
                    sha1: None,
                    sha256: None,
                    acquisition: vec![],
                    added_at: 0,
                })?;
                for (i, name) in ["a.txt", "B.doc", "c_1.txt", "d%.txt"].iter().enumerate() {
                    let mut f = NewFile::new(
                        ds,
                        None,
                        "/dir/",
                        *name,
                        Origin::Local,
                        FileKindCode::Regular,
                    );
                    f.size = 10 * (i as u64 + 1);
                    f.times = [
                        Some((1000 + i as i64, 0)),
                        None,
                        None,
                        Some((5000 - i as i64, 0)),
                    ];
                    b.add_file(&f)?;
                }
                Ok(ds)
            })
            .unwrap();
        (dir, db, ds)
    }

    #[test]
    fn read_only_open_refuses_writes() {
        let (dir, db, _) = db();
        drop(db);
        let ro = CaseDb::open_read_only(dir.path().join("case.db")).unwrap();
        assert!(ro.is_read_only());
        assert!(ro.set_meta("x", "y").is_err());
        assert_eq!(ro.mcp_file_count(None).unwrap(), 4);
    }

    #[test]
    fn file_paging_covers_every_row_once_in_each_order() {
        let (_d, db, _) = db();
        for order in [
            FileOrder::Id,
            FileOrder::Size,
            FileOrder::Name,
            FileOrder::Modified,
            FileOrder::Created,
        ] {
            for desc in [false, true] {
                let mut seen = Vec::new();
                let mut cur: Option<FileCursor> = None;
                loop {
                    let page = db
                        .mcp_files(&FileFilter::default(), order, desc, cur.as_ref(), 1)
                        .unwrap();
                    let Some(last) = page.last() else { break };
                    cur = Some(FileCursor {
                        key: order.key(last),
                        id: last.id,
                    });
                    seen.extend(page.iter().map(|f| f.id));
                }
                seen.sort_unstable();
                seen.dedup();
                assert_eq!(seen.len(), 4, "{order:?} desc={desc}");
            }
        }
    }

    #[test]
    fn like_wildcards_are_literal() {
        let (_d, db, _) = db();
        let f = FileFilter {
            name_contains: Some("%".into()),
            ..Default::default()
        };
        let rows = db.mcp_files(&f, FileOrder::Id, false, None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "d%.txt");
        let f = FileFilter {
            name_contains: Some("_".into()),
            ..Default::default()
        };
        assert_eq!(
            db.mcp_files(&f, FileOrder::Id, false, None, 10)
                .unwrap()
                .len(),
            1
        );
        let f = FileFilter {
            name_contains: Some("b.D".into()),
            ..Default::default()
        };
        assert_eq!(
            db.mcp_files(&f, FileOrder::Id, false, None, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn artifact_attribute_filters() {
        let (_d, db, ds) = db();
        let url = crate::catalog::attribute_type_id("TSK_URL").unwrap();
        let dt = crate::catalog::attribute_type_id("TSK_DATETIME").unwrap();
        for (u, t) in [("http://a.example/x", 100), ("http://b.example/y", 200)] {
            db.add_artifact(&NewArtifact {
                type_id: 4,
                ds_id: ds,
                file_id: None,
                source_artifact_id: None,
                score: None,
                conclusion: None,
                configuration: None,
                justification: None,
                created_at: 0,
                attributes: vec![
                    (url, Value::Text(u.into()), "t".into()),
                    (dt, Value::DateTime(t), "t".into()),
                ],
            })
            .unwrap();
        }
        let f = ArtifactFilter {
            attrs: vec![AttrPredicate::TextContains {
                type_id: Some(url),
                text: "B.EXAMPLE".into(),
            }],
            ..Default::default()
        };
        assert_eq!(db.mcp_artifacts(&f, None, false, 10).unwrap().len(), 1);
        let f = ArtifactFilter {
            analysis: Some(false),
            attrs: vec![AttrPredicate::IntRange {
                type_ids: vec![dt],
                from: Some(150),
                to: None,
            }],
            ..Default::default()
        };
        assert_eq!(db.mcp_artifacts(&f, None, false, 10).unwrap().len(), 1);
        let counts = db.mcp_artifact_type_counts(None).unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].count, 2);
        assert_eq!(
            db.artifact_type_by_name("tsk_web_history")
                .unwrap()
                .unwrap()
                .0,
            4
        );
    }

    #[test]
    fn file_events_merge_keysets() {
        let (_d, db, _) = db();
        let f = EventFilter::default();
        let all = db
            .mcp_file_events(TimeColumn::Modified, &f, EventKeyset::Start, false, 10)
            .unwrap();
        assert_eq!(all.len(), 4);
        let rest = db
            .mcp_file_events(
                TimeColumn::Modified,
                &f,
                EventKeyset::After {
                    time: 1001,
                    id: all[1].id,
                },
                false,
                10,
            )
            .unwrap();
        assert_eq!(rest.len(), 2);
        let h = db
            .mcp_file_event_histogram(TimeColumn::Created, &f, 4990, 5)
            .unwrap();
        assert_eq!(h.iter().map(|x| x.1).sum::<u64>(), 4);
    }
}

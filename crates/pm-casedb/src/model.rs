//! Typed records exchanged with the case database.

use rusqlite::Row;

use crate::opt_u64;

/// Kind of data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSourceKind {
    DiskImage = 0,
    LocalDisk = 1,
    LogicalFiles = 2,
    UnallocatedImage = 3,
}

impl DataSourceKind {
    fn from_code(c: i64) -> Self {
        match c {
            1 => Self::LocalDisk,
            2 => Self::LogicalFiles,
            3 => Self::UnallocatedImage,
            _ => Self::DiskImage,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDataSource {
    pub name: String,
    pub kind: DataSourceKind,
    pub paths: Vec<String>,
    pub size: u64,
    pub sector_size: u32,
    pub timezone: String,
    pub image_format: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub acquisition: Vec<(String, String)>,
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSourceRecord {
    pub id: i64,
    pub name: String,
    pub kind: DataSourceKind,
    pub paths: Vec<String>,
    pub size: u64,
    pub sector_size: u32,
    pub timezone: String,
    pub image_format: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub acquisition: Vec<(String, String)>,
    pub added_at: i64,
}

impl DataSourceRecord {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let paths: String = r.get(3)?;
        let acq: Option<String> = r.get(11)?;
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: DataSourceKind::from_code(r.get(2)?),
            paths: serde_json::from_str(&paths).unwrap_or_default(),
            size: opt_u64(r, 4)?.unwrap_or(0),
            sector_size: r.get(5)?,
            timezone: r.get(6)?,
            image_format: r.get(7)?,
            md5: r.get(8)?,
            sha1: r.get(9)?,
            sha256: r.get(10)?,
            acquisition: acq
                .and_then(|a| serde_json::from_str(&a).ok())
                .unwrap_or_default(),
            added_at: r.get(12)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVolume {
    pub addr: u32,
    /// 0 allocated, 1 unallocated, 2 meta.
    pub kind: i64,
    pub start_sector: u64,
    pub sector_count: u64,
    pub description: String,
    pub name: Option<String>,
    pub flags: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRecord {
    pub id: i64,
    pub addr: u32,
    pub kind: i64,
    pub start_sector: u64,
    pub sector_count: u64,
    pub description: String,
    pub name: Option<String>,
    pub flags: u64,
    pub sector_size: u32,
}

impl VolumeRecord {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            addr: r.get(1)?,
            kind: r.get(2)?,
            start_sector: opt_u64(r, 3)?.unwrap_or(0),
            sector_count: opt_u64(r, 4)?.unwrap_or(0),
            description: r.get(5)?,
            name: r.get(6)?,
            flags: opt_u64(r, 7)?.unwrap_or(0),
            sector_size: r.get(8)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFileSystem {
    pub ds_id: i64,
    pub volume_id: Option<i64>,
    pub kind: String,
    pub byte_offset: u64,
    pub block_size: u32,
    pub block_count: u64,
    pub root_inode: u64,
    pub label: Option<String>,
    pub serial: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemRecord {
    pub id: i64,
    pub ds_id: i64,
    pub volume_id: Option<i64>,
    pub kind: String,
    pub byte_offset: u64,
    pub block_size: u32,
    pub block_count: u64,
    pub root_inode: u64,
    pub label: Option<String>,
    pub serial: Option<String>,
}

impl FileSystemRecord {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            ds_id: r.get(1)?,
            volume_id: r.get(2)?,
            kind: r.get(3)?,
            byte_offset: opt_u64(r, 4)?.unwrap_or(0),
            block_size: r.get(5)?,
            block_count: opt_u64(r, 6)?.unwrap_or(0),
            root_inode: opt_u64(r, 7)?.unwrap_or(0),
            label: r.get(8)?,
            serial: r.get(9)?,
        })
    }
}

/// How a file came to be in the case (Autopsy's `TSK_DB_FILES_TYPE_ENUM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    FileSystem = 0,
    Carved = 1,
    Derived = 2,
    Local = 3,
    UnallocBlocks = 4,
    VirtualDir = 5,
    Slack = 6,
}

impl Origin {
    pub fn from_code(c: i64) -> Self {
        match c {
            1 => Self::Carved,
            2 => Self::Derived,
            3 => Self::Local,
            4 => Self::UnallocBlocks,
            5 => Self::VirtualDir,
            6 => Self::Slack,
            _ => Self::FileSystem,
        }
    }
}

/// File type codes stored in `files.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKindCode {
    Unknown = 0,
    Regular = 1,
    Directory = 2,
    Symlink = 3,
    CharDevice = 4,
    BlockDevice = 5,
    Fifo = 6,
    Socket = 7,
    Virtual = 8,
}

impl FileKindCode {
    pub fn from_code(c: i64) -> Self {
        match c {
            1 => Self::Regular,
            2 => Self::Directory,
            3 => Self::Symlink,
            4 => Self::CharDevice,
            5 => Self::BlockDevice,
            6 => Self::Fifo,
            7 => Self::Socket,
            8 => Self::Virtual,
            _ => Self::Unknown,
        }
    }
}

/// Known status (hash-set lookup / notable tagging).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Known {
    Unknown = 0,
    Known = 1,
    Notable = 2,
}

impl Known {
    pub fn from_code(c: i64) -> Self {
        match c {
            1 => Self::Known,
            2 => Self::Notable,
            _ => Self::Unknown,
        }
    }
}

/// A file to insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    /// Pre-assigned id (lets parallel producers know parent ids before the
    /// single writer inserts); `None` lets SQLite assign one.
    pub id: Option<i64>,
    pub ds_id: i64,
    pub fs_id: Option<i64>,
    pub parent_id: Option<i64>,
    pub origin: Origin,
    pub inode: Option<u64>,
    pub stream: Option<u32>,
    pub name: String,
    /// Parent path, `/`-terminated.
    pub parent_path: String,
    pub kind: FileKindCode,
    pub name_alloc: bool,
    pub meta_alloc: bool,
    pub meta_flags: u32,
    pub size: u64,
    /// modified, accessed, changed, created as (unix seconds, nanoseconds).
    pub times: [Option<(i64, u32)>; 4],
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub seq: Option<u32>,
    pub attributes: Option<u32>,
    pub derived_from: Option<i64>,
    pub local_path: Option<String>,
    /// JSON byte ranges for unallocated-space and carved files.
    pub layout: Option<String>,
}

/// A file row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: i64,
    pub ds_id: i64,
    pub fs_id: Option<i64>,
    pub parent_id: Option<i64>,
    pub origin: Origin,
    pub inode: Option<u64>,
    pub stream: Option<u32>,
    pub name: String,
    pub parent_path: String,
    pub kind: FileKindCode,
    pub name_alloc: bool,
    pub meta_alloc: bool,
    pub meta_flags: u32,
    pub size: u64,
    pub times: [Option<(i64, u32)>; 4],
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub seq: Option<u32>,
    pub attributes: Option<u32>,
    pub extension: String,
    pub mime: Option<String>,
    pub md5: Option<Vec<u8>>,
    pub sha1: Option<Vec<u8>>,
    pub sha256: Option<Vec<u8>>,
    pub known: Known,
    pub derived_from: Option<i64>,
    pub local_path: Option<String>,
}

impl FileRecord {
    pub fn path(&self) -> String {
        format!("{}{}", self.parent_path, self.name)
    }

    pub fn deleted(&self) -> bool {
        !self.name_alloc || !self.meta_alloc
    }

    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let time = |s: usize| -> rusqlite::Result<Option<(i64, u32)>> {
            let secs: Option<i64> = r.get(s)?;
            let ns: Option<i64> = r.get(s + 1)?;
            Ok(secs.map(|v| (v, ns.unwrap_or(0).clamp(0, 999_999_999) as u32)))
        };
        Ok(Self {
            id: r.get(0)?,
            ds_id: r.get(1)?,
            fs_id: r.get(2)?,
            parent_id: r.get(3)?,
            origin: Origin::from_code(r.get(4)?),
            inode: opt_u64(r, 5)?,
            stream: r.get(6)?,
            name: r.get(7)?,
            parent_path: r.get(8)?,
            kind: FileKindCode::from_code(r.get(9)?),
            name_alloc: r.get(10)?,
            meta_alloc: r.get(11)?,
            meta_flags: r.get(12)?,
            size: opt_u64(r, 13)?.unwrap_or(0),
            times: [time(14)?, time(16)?, time(18)?, time(20)?],
            mode: r.get(22)?,
            uid: r.get(23)?,
            gid: r.get(24)?,
            seq: r.get(25)?,
            attributes: r.get(26)?,
            extension: r.get(27)?,
            mime: r.get(28)?,
            md5: r.get(29)?,
            sha1: r.get(30)?,
            sha256: r.get(31)?,
            known: Known::from_code(r.get(32)?),
            derived_from: r.get(33)?,
            local_path: r.get(34)?,
        })
    }
}

/// Conditions for [`crate::CaseDb::query_files`] (all are `ANDed`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileQuery {
    pub ds_id: Option<i64>,
    pub regular_only: bool,
    pub extensions: Vec<String>,
    pub mime_prefix: Option<String>,
    pub deleted_only: bool,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub known: Option<Known>,
    pub md5: Option<Vec<u8>>,
    pub name_contains: Option<String>,
}

impl FileQuery {
    /// SQL condition and its arguments; placeholders are numbered after
    /// `reserved` leading parameters (e.g. the keyset cursor).
    pub(crate) fn where_clause(&self, reserved: usize) -> (String, Vec<rusqlite::types::Value>) {
        use rusqlite::types::Value;
        let mut parts = vec!["1=1".to_owned()];
        let mut args: Vec<Value> = Vec::new();
        let next = |v: Value, args: &mut Vec<Value>| {
            args.push(v);
            format!("?{}", args.len() + reserved)
        };
        if let Some(ds) = self.ds_id {
            let p = next(ds.into(), &mut args);
            parts.push(format!("ds_id = {p}"));
        }
        if self.regular_only {
            parts.push("kind = 1".into());
        }
        if !self.extensions.is_empty() {
            let ps: Vec<String> = self
                .extensions
                .iter()
                .map(|e| next(e.to_lowercase().into(), &mut args))
                .collect();
            parts.push(format!("extension IN ({})", ps.join(", ")));
        }
        if let Some(m) = &self.mime_prefix {
            let p = next(format!("{m}%").into(), &mut args);
            parts.push(format!("mime LIKE {p}"));
        }
        if self.deleted_only {
            parts.push("(name_alloc = 0 OR meta_alloc = 0)".into());
        }
        if let Some(v) = self.min_size {
            let p = next((v as i64).into(), &mut args);
            parts.push(format!("size >= {p}"));
        }
        if let Some(v) = self.max_size {
            let p = next((v as i64).into(), &mut args);
            parts.push(format!("size <= {p}"));
        }
        if let Some(k) = self.known {
            let p = next((k as i64).into(), &mut args);
            parts.push(format!("known = {p}"));
        }
        if let Some(h) = &self.md5 {
            let p = next(h.clone().into(), &mut args);
            parts.push(format!("md5 = {p}"));
        }
        if let Some(n) = &self.name_contains {
            let p = next(format!("%{n}%").into(), &mut args);
            parts.push(format!("name LIKE {p}"));
        }
        (parts.join(" AND "), args)
    }
}

/// Ingest results for one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileAnalysis {
    pub mime: Option<String>,
    pub md5: Option<Vec<u8>>,
    pub sha1: Option<Vec<u8>>,
    pub sha256: Option<Vec<u8>>,
    pub known: Option<Known>,
}

impl FileAnalysis {
    pub(crate) fn apply(&self, c: &rusqlite::Connection, id: i64) -> crate::Result<()> {
        c.prepare_cached(
            "UPDATE files SET mime = COALESCE(?2, mime), md5 = COALESCE(?3, md5), sha1 = COALESCE(?4, sha1), sha256 = COALESCE(?5, sha256), known = COALESCE(?6, known) WHERE id = ?1",
        )?
        .execute(rusqlite::params![id, self.mime, self.md5, self.sha1, self.sha256, self.known.map(|k| k as i64)])?;
        Ok(())
    }
}

/// Analysis-result significance (ordered; higher is more significant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Significance {
    Unknown = 0,
    LikelyNone = 1,
    None = 2,
    LikelyNotable = 3,
    Notable = 4,
}

impl Significance {
    pub fn from_code(c: i64) -> Option<Self> {
        Some(match c {
            0 => Self::Unknown,
            1 => Self::LikelyNone,
            2 => Self::None,
            3 => Self::LikelyNotable,
            4 => Self::Notable,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::LikelyNone => "Likely Not Notable",
            Self::None => "Not Notable",
            Self::LikelyNotable => "Likely Notable",
            Self::Notable => "Notable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Priority {
    Normal = 0,
    Override = 1,
}

/// An attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Int(i64),
    Double(f64),
    Bytes(Vec<u8>),
    /// Unix seconds.
    DateTime(i64),
    Json(String),
}

impl Value {
    #[allow(clippy::type_complexity, reason = "maps to four nullable columns")]
    pub(crate) fn columns(&self) -> (Option<&str>, Option<i64>, Option<f64>, Option<&[u8]>) {
        match self {
            Self::Text(s) | Self::Json(s) => (Some(s), None, None, None),
            Self::Int(i) | Self::DateTime(i) => (None, Some(*i), None, None),
            Self::Double(d) => (None, None, Some(*d), None),
            Self::Bytes(b) => (None, None, None, Some(b)),
        }
    }

    /// Display form (`DateTime` stays numeric; the UI formats times).
    pub fn display(&self) -> String {
        match self {
            Self::Text(s) | Self::Json(s) => s.clone(),
            Self::Int(i) | Self::DateTime(i) => i.to_string(),
            Self::Double(d) => d.to_string(),
            Self::Bytes(b) => b.iter().fold(String::new(), |mut s, x| {
                use std::fmt::Write as _;
                let _ = write!(s, "{x:02x}");
                s
            }),
        }
    }
}

/// An artifact or analysis result to insert.
#[derive(Debug, Clone, PartialEq)]
pub struct NewArtifact {
    pub type_id: i64,
    pub ds_id: i64,
    pub file_id: Option<i64>,
    pub source_artifact_id: Option<i64>,
    /// Analysis results only.
    pub score: Option<(Significance, Priority)>,
    pub conclusion: Option<String>,
    pub configuration: Option<String>,
    pub justification: Option<String>,
    pub created_at: i64,
    /// `(attribute type id, value, producing module)`.
    pub attributes: Vec<(i64, Value, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub id: i64,
    pub type_id: i64,
    pub ds_id: i64,
    pub file_id: Option<i64>,
    pub source_artifact_id: Option<i64>,
    pub significance: Option<Significance>,
    pub priority: Option<i64>,
    pub conclusion: Option<String>,
    pub configuration: Option<String>,
    pub justification: Option<String>,
    pub review_status: i64,
    pub created_at: i64,
}

impl ArtifactRecord {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            type_id: r.get(1)?,
            ds_id: r.get(2)?,
            file_id: r.get(3)?,
            source_artifact_id: r.get(4)?,
            significance: r
                .get::<_, Option<i64>>(5)?
                .and_then(Significance::from_code),
            priority: r.get(6)?,
            conclusion: r.get(7)?,
            configuration: r.get(8)?,
            justification: r.get(9)?,
            review_status: r.get(10)?,
            created_at: r.get(11)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttributeRecord {
    pub type_id: i64,
    pub name: String,
    pub display_name: String,
    pub value: Value,
    pub source: String,
}

impl AttributeRecord {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        use crate::catalog::ValueType as V;
        let vt = V::from_code(r.get(3)?);
        let text: Option<String> = r.get(5)?;
        let int: Option<i64> = r.get(6)?;
        let dbl: Option<f64> = r.get(7)?;
        let blob: Option<Vec<u8>> = r.get(8)?;
        let value = match vt {
            Some(V::DateTime) => Value::DateTime(int.unwrap_or(0)),
            Some(V::Integer | V::Long) => Value::Int(int.unwrap_or(0)),
            Some(V::Double) => Value::Double(dbl.unwrap_or(0.0)),
            Some(V::Bytes) => Value::Bytes(blob.unwrap_or_default()),
            Some(V::Json) => Value::Json(text.unwrap_or_default()),
            Some(V::String) | None => Value::Text(text.unwrap_or_default()),
        };
        Ok(Self {
            type_id: r.get(0)?,
            name: r.get(1)?,
            display_name: r.get(2)?,
            value,
            source: r.get(4)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagName {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub color: String,
    pub notable: bool,
}

impl TagName {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            description: r.get(2)?,
            color: r.get(3)?,
            notable: r.get(4)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTag {
    pub id: i64,
    pub file_id: i64,
    pub tag_name_id: i64,
    pub tag_name: String,
    pub comment: String,
    pub examiner: String,
    pub created_at: i64,
}

impl FileTag {
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            file_id: r.get(1)?,
            tag_name_id: r.get(2)?,
            tag_name: r.get(3)?,
            comment: r.get(4)?,
            examiner: r.get(5)?,
            created_at: r.get(6)?,
        })
    }
}

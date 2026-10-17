//! The postmortem module API (ADR-022).
//!
//! One contract for every analyzer that is not part of the core
//! framework: built-in parsers implement these traits natively, and
//! WebAssembly plugins implement the same traits through `pm-plugin-sdk`,
//! which carries them across the component boundary (the WIT world in
//! `wit/plugin.wit` mirrors this crate one to one).
//!
//! A module sees the case only through [`Host`]: it reads file content,
//! looks files up, posts data artifacts and analysis results, adds derived
//! files and writes to the log and the ingest inbox. It never touches the
//! case database, the file system or the network directly, so a plugin can
//! be sandboxed and the host can batch writes and run modules in parallel.
//!
//! Versioning: [`API_VERSION`] follows semver. Minor versions only add
//! (new host calls, new optional fields); a plugin built against `0.1`
//! runs on any `0.x` host with `x >= 1`.

use std::io;

use serde::{Deserialize, Serialize};

mod filter;

pub use filter::FileFilter;

/// The API version this crate implements.
pub const API_VERSION: Version = Version { major: 0, minor: 3 };

/// The oldest API version the host still runs: 0.3 renamed every result
/// and attribute type to the `KAAMOS_` namespace, so modules built for
/// older versions post names the host no longer knows.
pub const OLDEST_SUPPORTED: Version = Version { major: 0, minor: 3 };

/// A `major.minor` API version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

impl Version {
    /// Whether a module built against `self` runs on a host that
    /// implements `host`.
    pub fn runs_on(self, host: Self) -> bool {
        self.major == host.major && self.minor <= host.minor && self >= OLDEST_SUPPORTED
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// A module failure; the pipeline records it and continues with the next
/// file (Autopsy's `IngestModuleException`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PluginError(pub String);

impl PluginError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl From<io::Error> for PluginError {
    fn from(e: io::Error) -> Self {
        Self(e.to_string())
    }
}

pub type Result<T, E = PluginError> = std::result::Result<T, E>;

/// Identity and capabilities of a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Stable id used in settings and job records (`"chrome_history"`).
    pub id: String,
    /// Display name in the ingest module list.
    pub name: String,
    pub description: String,
    pub version: String,
    /// Selected when the job settings do not mention the module.
    pub enabled_by_default: bool,
    /// The files [`FileIngest::process`] is called for (`None`: the module
    /// has no per-file work). The host applies it, so unwanted files never
    /// cross into the module.
    pub file_filter: Option<FileFilter>,
    /// The stage [`DataSourceIngest::process`] runs in (`None`: no
    /// data-source work). Stage 1 runs alongside file analysis; later
    /// stages start once all earlier work has finished.
    pub data_source_stage: Option<u32>,
}

/// What kind of object a file row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileKind {
    /// A regular file (or an alternate data stream) from a file system.
    File,
    Directory,
    /// Unallocated blocks or slack.
    Unallocated,
    /// Recovered by a carver.
    Carved,
    /// Extracted from another file (archive member, attachment, …).
    Derived,
    /// Anything else (virtual directories, links, …).
    Other,
}

/// A file as a module sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    /// Case object id; stable for the life of the case.
    pub id: i64,
    pub ds_id: i64,
    pub name: String,
    /// Parent folder path with a trailing `/` (`/Users/alice/`).
    pub parent_path: String,
    /// Lower-case extension without the dot (empty when none).
    pub extension: String,
    pub size: u64,
    pub kind: FileKind,
    /// Name or metadata unallocated.
    pub deleted: bool,
    /// Detected by File Type Identification (runs before most modules).
    pub mime: Option<String>,
    /// Lower-case hex, when Hash Lookup has run.
    pub md5: Option<String>,
    /// Unix seconds.
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
    pub changed: Option<i64>,
}

impl FileInfo {
    /// `parent_path` + `name`.
    pub fn path(&self) -> String {
        format!("{}{}", self.parent_path, self.name)
    }
}

/// The data source a job runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSourceInfo {
    pub id: i64,
    pub name: String,
    /// IANA time zone of the evidence (`"Europe/Helsinki"`).
    pub timezone: String,
    pub size: u64,
}

/// Criteria for [`Host::find_files`] (Autopsy's `FileManager.findFiles`).
/// Unset criteria match everything; only files of the job's data source
/// with content are returned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindFiles {
    /// Whole name, case-insensitive.
    pub name: Option<String>,
    /// Lower-case extension without the dot.
    pub extension: Option<String>,
    /// Case-insensitive substring of the parent path (`"/AppData/"`).
    pub path_contains: Option<String>,
    /// Exact MIME type.
    pub mime: Option<String>,
    /// At most this many files (the host caps it at [`MAX_FIND_FILES`]).
    pub limit: Option<u32>,
}

/// The most files one [`Host::find_files`] call returns.
pub const MAX_FIND_FILES: u32 = 100_000;

/// An attribute value (Autopsy's value types).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Text(String),
    /// 32- and 64-bit integers alike.
    Int(i64),
    Double(f64),
    Bytes(Vec<u8>),
    /// Unix seconds.
    Time(i64),
}

/// One attribute of a result: a Autopsy attribute type name (`"KAAMOS_URL"`)
/// and its value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attribute {
    pub kind: String,
    pub value: Value,
}

impl Attribute {
    pub fn new(kind: impl Into<String>, value: Value) -> Self {
        Self {
            kind: kind.into(),
            value,
        }
    }

    pub fn text(kind: impl Into<String>, v: impl Into<String>) -> Self {
        Self::new(kind, Value::Text(v.into()))
    }

    pub fn int(kind: impl Into<String>, v: i64) -> Self {
        Self::new(kind, Value::Int(v))
    }

    pub fn time(kind: impl Into<String>, unix_seconds: i64) -> Self {
        Self::new(kind, Value::Time(unix_seconds))
    }
}

/// Autopsy's score significance (analysis results only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Significance {
    Unknown,
    LikelyNone,
    LikelyNotable,
    None,
    Notable,
}

/// A data artifact or analysis result to post. The category (artifact or
/// result) comes from the type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewResult {
    /// Autopsy artifact type name (`"KAAMOS_WEB_HISTORY"`).
    pub artifact_type: String,
    /// The source file (`None`: the data source itself).
    pub file_id: Option<i64>,
    /// Analysis results: the score.
    pub significance: Option<Significance>,
    pub conclusion: Option<String>,
    pub configuration: Option<String>,
    pub justification: Option<String>,
    pub attributes: Vec<Attribute>,
}

impl NewResult {
    /// A result of `artifact_type` on `file_id`, without attributes yet.
    pub fn new(artifact_type: impl Into<String>, file_id: Option<i64>) -> Self {
        Self {
            artifact_type: artifact_type.into(),
            file_id,
            significance: None,
            conclusion: None,
            configuration: None,
            justification: None,
            attributes: Vec::new(),
        }
    }

    /// Adds an attribute (builder style).
    #[must_use]
    pub fn with(mut self, a: Attribute) -> Self {
        self.attributes.push(a);
        self
    }
}

/// A file extracted from another one (an archive member, an attachment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedFile {
    pub name: String,
    /// The content (the host stores it under the case's module output).
    pub data: Vec<u8>,
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
}

/// The most bytes one [`DerivedFile`] may carry.
pub const MAX_DERIVED_FILE: usize = 256 * 1024 * 1024;

/// An account as a communication names it (Autopsy `Account.Type` name and
/// identifier, e.g. `EMAIL` / `bob@example.com`; the host normalises it).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountId {
    pub account_type: String,
    pub id: String,
}

impl AccountId {
    pub fn new(account_type: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            account_type: account_type.into(),
            id: id.into(),
        }
    }
}

/// Autopsy `Relationship.Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelationshipKind {
    Message,
    CallLog,
    Contact,
}

/// The accounts a communication result involves (API 0.2): each is
/// registered on the result's file, and `relationship` relates the
/// participants through the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Communication {
    pub accounts: Vec<AccountId>,
    /// `(indexes into accounts, kind, time)`.
    pub relationship: Option<(Vec<usize>, RelationshipKind, Option<i64>)>,
}

/// How a custom attribute's value is stored (Autopsy value types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ValueKind {
    String,
    Integer,
    Long,
    Double,
    Bytes,
    DateTime,
    Json,
}

/// Whether a custom artifact type is a data artifact or an analysis
/// result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtifactCategory {
    DataArtifact,
    AnalysisResult,
}

/// Log levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

/// An ingest inbox message (Autopsy's `IngestMessage`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub level: Level,
    pub subject: String,
    pub details: String,
    /// The file the message is about.
    pub file_id: Option<i64>,
}

/// What a module may do with the case. Calls are cheap to make often:
/// posts are batched by the host, and content reads go through the host's
/// caches.
pub trait Host {
    /// The data source of the job.
    fn data_source(&self) -> DataSourceInfo;

    /// Reads file content at `offset` into `buf`; returns the bytes read
    /// (0 at the end).
    fn read(&self, file_id: i64, offset: u64, buf: &mut [u8]) -> Result<usize>;

    /// A file of the case by id.
    fn file(&self, file_id: i64) -> Result<Option<FileInfo>>;

    /// Files of the job's data source matching `q`, by id.
    fn find_files(&self, q: &FindFiles) -> Result<Vec<FileInfo>>;

    /// The children of a directory (or of a file with derived files).
    fn children(&self, file_id: i64) -> Result<Vec<FileInfo>>;

    /// Posts a data artifact or analysis result. Posting the same result
    /// again on a re-run is a no-op.
    fn post(&self, result: NewResult) -> Result<()>;

    /// Adds files extracted from `parent_id` and schedules them for
    /// analysis; returns their ids (a re-run returns the existing ids).
    fn add_derived(&self, parent_id: i64, files: Vec<DerivedFile>) -> Result<Vec<i64>>;

    fn log(&self, level: Level, message: &str);

    /// Posts a message to the ingest inbox.
    fn message(&self, message: Message);

    /// The job was cancelled: return as soon as possible.
    fn is_cancelled(&self) -> bool;

    /// Posts a communication result (an e-mail, message, call or contact)
    /// together with its accounts and relationship, atomically (API 0.2).
    /// Re-runs add nothing.
    fn post_communication(&self, result: NewResult, communication: Communication) -> Result<()> {
        let _ = (result, communication);
        Err(PluginError::new(
            "this host does not support communications",
        ))
    }

    /// Registers (or finds) a custom attribute type that results may then
    /// use by name (Autopsy `getOrAddAttributeType`; API 0.2).
    fn define_attribute_type(&self, name: &str, display: &str, kind: ValueKind) -> Result<()> {
        let _ = (name, display, kind);
        Err(PluginError::new(
            "this host does not support custom attribute types",
        ))
    }

    /// Registers (or finds) a custom artifact type (Autopsy
    /// `getOrAddArtifactType`; API 0.2).
    fn define_artifact_type(
        &self,
        name: &str,
        display: &str,
        category: ArtifactCategory,
    ) -> Result<()> {
        let _ = (name, display, category);
        Err(PluginError::new(
            "this host does not support custom artifact types",
        ))
    }

    /// A handle to this host that can be kept for the rest of the job
    /// (runtimes such as the WebAssembly host must own what they call).
    fn shared(&self) -> std::sync::Arc<dyn Host + Send + Sync>;
}

/// Per-file analysis. One instance runs on one worker thread; the host
/// creates as many as it has workers.
pub trait FileIngest: Send {
    fn process(&mut self, host: &dyn Host, file: &FileInfo) -> Result<()>;

    /// Called once when the worker finishes (also after cancellation).
    fn finish(&mut self, _host: &dyn Host) {}
}

/// Whole-data-source analysis (runs once per job).
pub trait DataSourceIngest: Send {
    fn process(&mut self, host: &dyn Host) -> Result<()>;
}

/// A module: creates per-job instances.
pub trait Plugin: Send + Sync {
    fn info(&self) -> PluginInfo;

    /// Settings used when a job does not provide any.
    fn default_settings(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Checks the settings before the job starts; an error aborts the job
    /// before any data is processed (Autopsy's `startUp` failure).
    fn validate(&self, _settings: &serde_json::Value) -> Result<()> {
        Ok(())
    }

    /// A per-file instance for one worker (`None`: no file work).
    fn file_ingest(&self, _settings: &serde_json::Value) -> Result<Option<Box<dyn FileIngest>>> {
        Ok(None)
    }

    /// The data-source instance of a job (`None`: no data-source work).
    fn data_source_ingest(
        &self,
        _settings: &serde_json::Value,
    ) -> Result<Option<Box<dyn DataSourceIngest>>> {
        Ok(None)
    }
}

/// [`io::Read`] + [`io::Seek`] over a file's content through the host.
#[derive(Debug)]
pub struct HostReader<'h, H: Host + ?Sized> {
    host: &'h H,
    file_id: i64,
    size: u64,
    pos: u64,
}

impl<'h, H: Host + ?Sized> HostReader<'h, H> {
    pub fn new(host: &'h H, file: &FileInfo) -> Self {
        Self {
            host,
            file_id: file.id,
            size: file.size,
            pos: 0,
        }
    }

    /// Reads the whole file if it is at most `max` bytes.
    pub fn read_all(&mut self, max: u64) -> Result<Vec<u8>> {
        if self.size > max {
            return Err(PluginError(format!(
                "file is {} bytes, more than the {max} allowed",
                self.size
            )));
        }
        let mut out = Vec::with_capacity(usize::try_from(self.size).unwrap_or(0));
        io::Read::read_to_end(self, &mut out)?;
        Ok(out)
    }
}

impl<H: Host + ?Sized> io::Read for HostReader<'_, H> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self
            .host
            .read(self.file_id, self.pos, buf)
            .map_err(|e| io::Error::other(e.0))?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<H: Host + ?Sized> io::Seek for HostReader<'_, H> {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        let pos = match to {
            io::SeekFrom::Start(p) => Some(p),
            io::SeekFrom::End(d) => self.size.checked_add_signed(d),
            io::SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = pos.ok_or_else(|| io::Error::other("seek before the start"))?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compatibility() {
        let v = |major, minor| Version { major, minor };
        assert!(v(0, 3).runs_on(v(0, 3)));
        assert!(v(0, 3).runs_on(v(0, 4)));
        assert!(!v(0, 2).runs_on(v(0, 3)), "pre-KAAMOS type names");
        assert!(!v(0, 4).runs_on(v(0, 3)), "needs newer host calls");
        assert!(!v(1, 0).runs_on(v(0, 9)));
        assert_eq!(API_VERSION.to_string(), "0.3");
    }
}

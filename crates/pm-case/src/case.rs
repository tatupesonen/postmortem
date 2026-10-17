//! Case folders, case metadata and evidence re-opening.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use pm_casedb::{CaseDb, DataSourceRecord};
use pm_core::SharedSource;
use pm_evidence::{Evidence, OpenOptions};
use pm_fs::FileSystem;
use serde::{Deserialize, Serialize};

use crate::{CaseError, Result, io_err};

/// Name of the case metadata file inside the case folder.
pub const CASE_FILE: &str = "case.pmcase";
/// Name of the case database inside the case folder.
pub const DB_FILE: &str = "case.db";
/// Sub-folders created in every case (as in Autopsy).
pub const SUBDIRS: &[&str] = &["Export", "Reports", "ModuleOutput", "Temp", "Log"];

mod read_only;

const FORMAT: &str = "postmortem-case";
const FORMAT_VERSION: u32 = 1;

/// Single-user (local SQLite) or multi-user (shared server) case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaseKind {
    #[default]
    SingleUser,
    MultiUser,
}

/// Examiner contact details.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Examiner {
    pub name: String,
    pub phone: String,
    pub email: String,
}

/// Editable case properties (Autopsy's `CaseDetails`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaseDetails {
    /// Display name.
    pub name: String,
    pub number: String,
    pub examiner: Examiner,
    pub notes: String,
}

/// On-disk form of `case.pmcase`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CaseFile {
    format: String,
    version: u32,
    id: String,
    kind: CaseKind,
    /// RFC 3339 creation time.
    created: String,
    created_by: String,
    #[serde(default)]
    details: CaseDetails,
}

/// Evidence opened for one data source, with case file-system ids mapped to
/// the parsed file systems.
#[derive(Debug)]
pub(crate) struct OpenSource {
    pub(crate) evidence: Evidence,
    pub(crate) filesystems: HashMap<i64, Arc<dyn FileSystem>>,
}

/// An open case.
#[derive(Debug)]
pub struct Case {
    dir: PathBuf,
    meta: RwLock<CaseFile>,
    db: Arc<CaseDb>,
    sources: Mutex<HashMap<i64, Arc<OpenSource>>>,
    /// The keyword and file-name indexes, opened on first use and kept
    /// open (with their writer) while the case is.
    index: Mutex<Option<Arc<pm_search::CaseIndex>>>,
}

impl Case {
    /// Creates a case in `dir` (created if missing; must be empty).
    pub fn create(dir: impl AsRef<Path>, details: CaseDetails) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        if dir.exists() {
            let mut entries = std::fs::read_dir(&dir).map_err(io_err(&dir))?;
            if entries.next().is_some() {
                return Err(CaseError::FolderNotEmpty(dir));
            }
        }
        std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        for sub in SUBDIRS {
            let p = dir.join(sub);
            std::fs::create_dir_all(&p).map_err(io_err(&p))?;
        }
        let meta = CaseFile {
            format: FORMAT.into(),
            version: FORMAT_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            kind: CaseKind::SingleUser,
            created: jiff::Timestamp::now().to_string(),
            created_by: format!("postmortem {}", env!("CARGO_PKG_VERSION")),
            details,
        };
        write_case_file(&dir, &meta)?;
        let db = CaseDb::create(dir.join(DB_FILE))?;
        db.set_meta("case_id", &meta.id)?;
        db.ensure_project_vic_tags()?;
        tracing::info!(dir = %dir.display(), "case created");
        Ok(Self {
            dir,
            meta: RwLock::new(meta),
            db: Arc::new(db),
            sources: Mutex::new(HashMap::new()),
            index: Mutex::new(None),
        })
    }

    /// Opens a case from its folder or its `case.pmcase` file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        let file = dir.join(CASE_FILE);
        let text = std::fs::read_to_string(&file).map_err(io_err(&file))?;
        let bad = |reason: String| CaseError::BadCaseFile {
            path: file.clone(),
            reason,
        };
        let meta: CaseFile = toml::from_str(&text).map_err(|e| bad(e.to_string()))?;
        if meta.format != FORMAT {
            return Err(bad(format!("unknown format {:?}", meta.format)));
        }
        if meta.version > FORMAT_VERSION {
            return Err(bad(format!(
                "case file version {} is newer than this postmortem supports ({FORMAT_VERSION})",
                meta.version
            )));
        }
        let db_path = dir.join(DB_FILE);
        if !db_path.is_file() {
            return Err(bad(format!("missing {DB_FILE}")));
        }
        let db = CaseDb::open(&db_path)?;
        db.ensure_project_vic_tags()?;
        for sub in SUBDIRS {
            let p = dir.join(sub);
            std::fs::create_dir_all(&p).map_err(io_err(&p))?;
        }
        tracing::info!(dir = %dir.display(), "case opened");
        Ok(Self {
            dir,
            meta: RwLock::new(meta),
            db: Arc::new(db),
            sources: Mutex::new(HashMap::new()),
            index: Mutex::new(None),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of `case.pmcase`.
    pub fn case_file(&self) -> PathBuf {
        self.dir.join(CASE_FILE)
    }

    pub fn db(&self) -> &Arc<CaseDb> {
        &self.db
    }

    pub fn id(&self) -> String {
        self.meta.read().id.clone()
    }

    pub fn kind(&self) -> CaseKind {
        self.meta.read().kind
    }

    pub fn created(&self) -> String {
        self.meta.read().created.clone()
    }

    pub fn details(&self) -> CaseDetails {
        self.meta.read().details.clone()
    }

    /// Updates the editable case properties and saves `case.pmcase`.
    pub fn set_details(&self, details: CaseDetails) -> Result<()> {
        let mut meta = self.meta.write();
        let mut updated = meta.clone();
        updated.details = details;
        write_case_file(&self.dir, &updated)?;
        *meta = updated;
        Ok(())
    }

    pub fn export_dir(&self) -> PathBuf {
        self.dir.join("Export")
    }

    pub fn reports_dir(&self) -> PathBuf {
        self.dir.join("Reports")
    }

    pub fn module_output_dir(&self) -> PathBuf {
        self.dir.join("ModuleOutput")
    }

    pub fn temp_dir(&self) -> PathBuf {
        self.dir.join("Temp")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.dir.join("Log")
    }

    /// Points a data source at moved evidence and forgets any opened copy.
    pub fn relocate_data_source(&self, ds_id: i64, paths: &[PathBuf]) -> Result<()> {
        let paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        self.db.set_data_source_paths(ds_id, &paths)?;
        self.sources.lock().remove(&ds_id);
        Ok(())
    }

    /// Removes a data source and everything derived from it.
    pub fn delete_data_source(&self, ds_id: i64) -> Result<()> {
        self.sources.lock().remove(&ds_id);
        self.db.delete_data_source(ds_id)?;
        // Autopsy's keyword search service deletes it from the index too.
        if self.dir.join(pm_search::INDEX_DIR).exists() {
            self.index()?.delete_data_source(ds_id)?;
        }
        Ok(())
    }

    /// The case's keyword and file-name indexes (created on first use).
    pub fn index(&self) -> Result<Arc<pm_search::CaseIndex>> {
        let mut ix = self.index.lock();
        if let Some(ix) = ix.as_ref() {
            return Ok(Arc::clone(ix));
        }
        let opened = pm_search::CaseIndex::open(&self.dir)?;
        *ix = Some(Arc::clone(&opened));
        Ok(opened)
    }

    /// The parsed file system with case id `fs_id`.
    pub fn filesystem(&self, fs_id: i64) -> Result<Arc<dyn FileSystem>> {
        let rec = self
            .db
            .file_system(fs_id)?
            .ok_or_else(|| CaseError::NotFound(format!("file system {fs_id}")))?;
        let src = self.source(rec.ds_id)?;
        src.filesystems
            .get(&fs_id)
            .cloned()
            .ok_or_else(|| CaseError::NotFound(format!("file system {fs_id} in the evidence")))
    }

    /// The logical content of a data source's image.
    pub fn image_source(&self, ds_id: i64) -> Result<SharedSource> {
        Ok(self.source(ds_id)?.evidence.image.source())
    }

    /// Opened evidence for `ds_id` (opened on first use, then cached).
    pub(crate) fn source(&self, ds_id: i64) -> Result<Arc<OpenSource>> {
        if let Some(s) = self.sources.lock().get(&ds_id) {
            return Ok(Arc::clone(s));
        }
        let rec = self
            .db
            .data_source(ds_id)?
            .ok_or_else(|| CaseError::NotFound(format!("data source {ds_id}")))?;
        let opened = Arc::new(self.reopen(&rec)?);
        // Another thread may have raced us; keep whichever landed first.
        let mut map = self.sources.lock();
        Ok(Arc::clone(map.entry(ds_id).or_insert(opened)))
    }

    /// Caches evidence the loader already parsed.
    pub(crate) fn register_source(&self, ds_id: i64, source: OpenSource) {
        self.sources.lock().insert(ds_id, Arc::new(source));
    }

    fn reopen(&self, rec: &DataSourceRecord) -> Result<OpenSource> {
        let paths: Vec<PathBuf> = rec.paths.iter().map(PathBuf::from).collect();
        let first = paths.first().cloned().unwrap_or_default();
        if !first.is_file() {
            return Err(CaseError::MissingEvidence {
                ds_id: rec.id,
                path: first,
            });
        }
        // The recorded segments, in the image's own format (E01/E02/… are
        // EWF segments, not a raw split image).
        let image = pm_image::open_segments(&paths).map_err(pm_evidence::EvidenceError::from)?;
        let evidence = pm_evidence::analyze(image, &open_options(&rec.timezone))?;
        let mut filesystems = HashMap::new();
        for fs in self.db.file_systems(rec.id)? {
            if let Some(found) = evidence
                .filesystems
                .iter()
                .find(|f| f.offset == fs.byte_offset)
            {
                filesystems.insert(fs.id, Arc::clone(&found.fs));
            } else {
                tracing::warn!(
                    fs_id = fs.id,
                    offset = fs.byte_offset,
                    "file system recorded in the case was not found in the evidence"
                );
            }
        }
        Ok(OpenSource {
            evidence,
            filesystems,
        })
    }
}

/// Evidence options for a data source's stored time zone.
pub(crate) fn open_options(timezone: &str) -> OpenOptions {
    OpenOptions {
        timezone: jiff::tz::TimeZone::get(timezone).unwrap_or(jiff::tz::TimeZone::UTC),
    }
}

fn write_case_file(dir: &Path, meta: &CaseFile) -> Result<()> {
    let text = toml::to_string_pretty(meta).map_err(|e| CaseError::BadCaseFile {
        path: dir.join(CASE_FILE),
        reason: e.to_string(),
    })?;
    // Write-then-rename so a crash never leaves a truncated case file.
    let tmp = dir.join(format!("{CASE_FILE}.tmp"));
    std::fs::write(&tmp, text).map_err(io_err(&tmp))?;
    let dest = dir.join(CASE_FILE);
    std::fs::rename(&tmp, &dest).map_err(io_err(&dest))?;
    Ok(())
}

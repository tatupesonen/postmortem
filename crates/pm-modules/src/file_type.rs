//! File Type Identification (Autopsy `modules/filetypeid`, inventory 03
//! §2.2).
//!
//! Sets every file's MIME type with Autopsy's `FileTypeDetector`
//! (`pm_filetype`): user-defined signatures, Autopsy's own signatures,
//! then Apache Tika 3.0.0 detection. A file matching a user-defined type
//! that asks for it gets a `KAAMOS_INTERESTING_ITEM` result (score Likely
//! Notable, configuration and `KAAMOS_SET_NAME` = the type's set name,
//! `KAAMOS_CATEGORY` = its MIME type). The job ends with Autopsy's "File Type
//! Id Results" summary.
//!
//! Directories and virtual directories get no MIME type (Autopsy stores
//! `application/octet-stream` on them, which nothing reads); other
//! non-regular objects, empty files, unallocated space and slack under
//! 4096 bytes get `application/octet-stream`, as in Autopsy.
//!
//! Later modules use [`mime_or_detect`] for Autopsy's
//! `FileTypeDetector.getMIMEType`: the stored type, or detection now when
//! File Type Identification has not run.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use pm_casedb::{FileKindCode, FileRecord, Origin, Significance};
use pm_filetype::{FileClass, FileType, FileTypeDetector, strip_params};
use pm_ingest::{
    FileModule, IngestFile, IngestMessage, JobContext, MessageKind, ModuleError, ModuleFactory,
    ModuleInfo, ModuleJob,
};
use serde::{Deserialize, Serialize};

use crate::util::Art;

pub const NAME: &str = "File Type Identification";
/// Bytes read for the interesting-file signature check
/// (`FileTypeIdIngestModule.detectUserDefinedFileType`).
const USER_TYPE_BUFFER: usize = 1024;

/// Per-job settings: the user-defined file types (Autopsy keeps them in
/// `UserFileTypeDefinitions.settings`; checked in order, first match
/// wins).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTypeSettings {
    #[serde(default)]
    pub user_types: Vec<FileType>,
}

#[derive(Debug, Default)]
pub struct FileTypeFactory;

impl ModuleFactory for FileTypeFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "file_type",
            name: NAME,
            description: "Matches file types based on binary signatures.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(FileTypeSettings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        _ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let settings: FileTypeSettings = if settings.is_null() {
            FileTypeSettings::default()
        } else {
            serde_json::from_value(settings.clone()).map_err(|e| {
                ModuleError::new(format!("Unable to run file type detection. ({e})"))
            })?
        };
        for t in &settings.user_types {
            t.validate().map_err(|e| {
                ModuleError::new(format!("Unable to run file type detection. ({e})"))
            })?;
        }
        Ok(Arc::new(JobRef(Arc::new(Job {
            detector: FileTypeDetector::new(settings.user_types),
            files: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
        }))))
    }
}

struct Job {
    detector: FileTypeDetector,
    /// `IngestJobTotals`.
    files: AtomicU64,
    nanos: AtomicU64,
}

struct JobRef(Arc<Job>);

impl ModuleJob for JobRef {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(Worker {
            job: Arc::clone(&self.0),
        }))
    }

    /// The "File Type Id Results" summary (Autopsy posts it when the last
    /// instance shuts down).
    fn finish(&self, ctx: &JobContext) {
        let job = &self.0;
        let ms = job.nanos.load(Ordering::Relaxed) / 1_000_000;
        ctx.post_message(
            IngestMessage::new(MessageKind::Info, NAME, "File Type Id Results").details(format!(
                "{NAME}\nTotal Processing Time: {ms} ms\nTotal Files Processed: {}",
                job.files.load(Ordering::Relaxed)
            )),
        );
    }
}

struct Worker {
    job: Arc<Job>,
}

impl FileModule for Worker {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        let start = Instant::now();
        let det = &self.job.detector;
        mime_or_detect(file, det);
        if !det.user_types().is_empty()
            && !is_directory(&file.rec)
            && let Ok(src) = file.content()
        {
            let head = file.head();
            let head = head.get(..head.len().min(USER_TYPE_BUFFER)).unwrap_or(head);
            if let Some(t) = det
                .user_type_of(&*src, head)
                .filter(|t| t.create_interesting_file_hit)
            {
                interesting_hit(ctx, file.rec.id, t);
            }
        }
        let job = &self.job;
        job.files.fetch_add(1, Ordering::Relaxed);
        job.nanos.fetch_add(
            u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        Ok(())
    }
}

/// `createInterestingFileHit` (posted once per file, type and set).
fn interesting_hit(ctx: &JobContext, file_id: i64, t: &FileType) {
    Art::result(
        ctx,
        NAME,
        "KAAMOS_INTERESTING_ITEM",
        file_id,
        Significance::LikelyNotable,
    )
    .configuration(t.interesting_files_set_name.clone())
    .text("KAAMOS_SET_NAME", t.interesting_files_set_name.clone())
    .text("KAAMOS_CATEGORY", t.mime_type.clone())
    .post(ctx, &["KAAMOS_SET_NAME", "KAAMOS_CATEGORY"]);
}

fn is_directory(rec: &FileRecord) -> bool {
    rec.kind == FileKindCode::Directory || rec.origin == Origin::VirtualDir
}

/// What Autopsy's detector knows about a file before reading it; `None`
/// for directories, which are not typed.
pub fn file_class(rec: &FileRecord) -> Option<FileClass> {
    if is_directory(rec) {
        return None;
    }
    Some(match rec.origin {
        Origin::UnallocBlocks => FileClass::Unallocated,
        Origin::Slack => FileClass::Slack,
        _ if rec.kind == FileKindCode::Regular => FileClass::File,
        _ => FileClass::Other,
    })
}

/// A detector without user-defined types, for modules that need a type
/// before File Type Identification has run.
pub fn default_detector() -> &'static FileTypeDetector {
    static DET: LazyLock<FileTypeDetector> = LazyLock::new(FileTypeDetector::default);
    &DET
}

/// Autopsy's `FileTypeDetector.getMIMEType`: the file's stored MIME type
/// (without parameters), or the detected one, which is then recorded on
/// the file. `None` for directories.
pub fn mime_or_detect(file: &mut IngestFile, det: &FileTypeDetector) -> Option<String> {
    if let Some(m) = file.mime() {
        return Some(strip_params(m).to_owned());
    }
    let class = file_class(&file.rec)?;
    let mime = match file.content() {
        Ok(src) => det.detect(class, &file.rec.name, &file.rec.extension, &*src),
        Err(e) => {
            // Autopsy: the read fails, Tika throws, octet-stream.
            tracing::debug!("cannot read file {} for type detection: {e}", file.rec.id);
            pm_filetype::media::OCTET_STREAM.to_owned()
        }
    };
    file.set_mime(mime.clone());
    Some(mime)
}

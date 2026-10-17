//! Embedded File Extractor (Autopsy `modules/embeddedfileextractor`,
//! inventory 03 §2.4).
//!
//! Archives (zip, 7z, RAR, tar, gzip, bzip2, xz, zstd; ISO 9660 images
//! when enabled) are unpacked with their directory tree; Office and PDF
//! documents yield their pictures, embedded objects and attachments. The
//! extracted files are stored XOR1-encoded under `ModuleOutput/EFE/<id>/`,
//! added as derived files and scheduled into the running job, so they go
//! through every module, recursively.
//!
//! The format comes from the MIME type File Type Identification set, or
//! from the content's signature when no type is known yet.
//!
//! **Zip bombs** (Autopsy `SevenZipExtractor`): every archive extracted
//! from another one remembers its nesting depth and the *root* archive it
//! came from. An archive at depth 4 flags the root, as does an item whose
//! declared size is at least 500 MB with a compression ratio of 600 or
//! more (the check is skipped for deleted archives and for
//! `application/x-gzip`, as in Autopsy). `pm-archive` additionally
//! applies the ratio to the bytes actually decoded, and caps members,
//! dictionaries and memory. Flagging posts `TSK_INTERESTING_ITEM` (set
//! "Possible Zip Bomb") on the root and a warning; archives below a
//! flagged root are skipped.
//!
//! **Encryption**: members that cannot be decrypted are listed without
//! content and the archive gets `TSK_ENCRYPTION_DETECTED` ("Password
//! protection detected." when every item is encrypted, else
//! "Content-only Encryption (Archive File)").
//!
//! Differences from Autopsy (deliberate):
//! * re-running ingest schedules only new or updated members (the others
//!   are already streamed by the job like every case file), and documents
//!   are not extracted a second time (Autopsy's skip is commented out, so
//!   it duplicates them);
//! * an archive whose header is encrypted (7z, RAR) gets the encryption
//!   result instead of only an error message;
//! * when the decoded-byte meter stops an archive part-way, the members
//!   completed before it are kept;
//! * xz, zstd and (optionally) ISO 9660 are extracted too.

mod archive;
mod document;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use pm_archive::Format;
use pm_casedb::{Known, Significance};
use pm_ingest::{
    FileModule, IngestFile, IngestMessage, JobContext, MessageKind, ModuleError, ModuleFactory,
    ModuleInfo, ModuleJob,
};
use serde::{Deserialize, Serialize};

use crate::derive::escape_file_name;
use crate::util::Art;

/// Module display name (Autopsy's identity key).
pub const NAME: &str = "Embedded File Extractor";
/// Output folder under `ModuleOutput` (Autopsy `getOutputFolderName`).
pub const OUTPUT_FOLDER: &str = "EFE";
/// Nesting depth at which the root archive is flagged (`MAX_DEPTH`).
pub const MAX_DEPTH: u32 = 4;
/// Interesting-item set name of zip bombs.
pub const ZIP_BOMB_SET: &str = "Possible Zip Bomb";
/// `TSK_ENCRYPTION_DETECTED` comment when every item is encrypted
/// (`EncryptionDetectionModuleFactory.PASSWORD_PROTECT_MESSAGE`).
pub const ENCRYPTION_FULL: &str = "Password protection detected.";
/// … when only some items are.
pub const ENCRYPTION_FILE_LEVEL: &str = "Content-only Encryption (Archive File)";

/// Per-job settings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Extract the files of ISO 9660 images found in the data source
    /// (Autopsy does not; disc images can be large).
    pub extract_disk_images: bool,
}

#[derive(Debug, Default)]
pub struct EmbeddedFileExtractorFactory;

impl ModuleFactory for EmbeddedFileExtractorFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "embedded_file_extractor",
            name: NAME,
            description: "Extracts embedded files (doc, docx, ppt, pptx, xls, xlsx, zip, rar, arj, 7z, gzip, bzip2, tar), schedules them for ingestion, and populates the directory tree with them.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(Settings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let settings: Settings = if settings.is_null() {
            Settings::default()
        } else {
            serde_json::from_value(settings.clone())
                .map_err(|e| ModuleError::new(format!("{NAME} settings: {e}")))?
        };
        let dir = ctx.case.dir().join("ModuleOutput").join(OUTPUT_FOLDER);
        std::fs::create_dir_all(&dir).map_err(|e| {
            ModuleError::new(format!(
                "Failed to create module output directory for Embedded File Extractor: {e}"
            ))
        })?;
        Ok(Arc::new(JobHandle(Arc::new(Job {
            settings,
            nesting: Mutex::new(HashMap::new()),
            flagged: Mutex::new(HashSet::new()),
        }))))
    }
}

/// The root archive of a nest of archives.
#[derive(Debug)]
pub(crate) struct Root {
    pub(crate) id: i64,
    /// `parent path + name` (Autopsy `getArchiveFilePath`).
    pub(crate) path: String,
}

/// Where an archive found inside another one sits.
#[derive(Debug, Clone)]
pub(crate) struct Nesting {
    pub(crate) depth: u32,
    pub(crate) root: Arc<Root>,
}

pub(crate) struct Job {
    settings: Settings,
    /// Archives extracted from archives, by file id, until they are
    /// processed (Autopsy's per-job depth map; entries are removed once
    /// used, so memory follows the archives waiting in the queue).
    nesting: Mutex<HashMap<i64, Nesting>>,
    /// Root archives flagged as possible zip bombs.
    flagged: Mutex<HashSet<i64>>,
}

/// The job state behind an `Arc`, shared by the worker instances.
struct JobHandle(Arc<Job>);

impl ModuleJob for JobHandle {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(Worker {
            job: Arc::clone(&self.0),
        }))
    }
}

struct Worker {
    job: Arc<Job>,
}

/// What a file is, for this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Archive(Format),
    Document(Format),
}

impl Job {
    /// Archive formats extracted with their tree.
    fn archive_enabled(&self, f: Format) -> bool {
        match f {
            Format::Iso9660 => self.settings.extract_disk_images,
            f => f.is_archive(),
        }
    }

    /// The module's view of a file: from its MIME type when known (File
    /// Type Identification), else from its signature.
    fn kind(&self, file: &IngestFile) -> Result<Option<Kind>, ModuleError> {
        let format = match file.mime() {
            Some(m) => pm_archive::format_for_mime(m),
            None => self.sniff(file)?,
        };
        Ok(format.and_then(|f| {
            if f.is_document() {
                Some(Kind::Document(f))
            } else if self.archive_enabled(f) {
                Some(Kind::Archive(f))
            } else {
                None
            }
        }))
    }

    /// Signature detection, touching the content beyond the cached head
    /// only for zip and compound files (Office refinement) and, when
    /// enabled, disc images.
    fn sniff(&self, file: &IngestFile) -> Result<Option<Format>, ModuleError> {
        const OLE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        let head = file.head();
        match pm_archive::detect_head(head) {
            Some(Format::Zip) => {}
            Some(f) => return Ok(Some(f)),
            None if head.starts_with(&OLE) => {}
            None if self.settings.extract_disk_images && file.rec.size > 34 * 1024 => {}
            None => return Ok(None),
        }
        let src = file.content()?;
        Ok(pm_archive::detect(&*src))
    }

    /// The nesting of an archive (depth 0 and itself as root unless it
    /// was extracted from another archive in this job).
    pub(crate) fn take_nesting(&self, file: &IngestFile) -> Nesting {
        self.nesting
            .lock()
            .remove(&file.rec.id)
            .unwrap_or_else(|| Nesting {
                depth: 0,
                root: Arc::new(Root {
                    id: file.rec.id,
                    path: file.rec.path(),
                }),
            })
    }

    /// Records an archive found inside another one.
    pub(crate) fn add_nested(&self, file_id: i64, nesting: Nesting) {
        self.nesting.lock().insert(file_id, nesting);
    }

    pub(crate) fn is_flagged(&self, root_id: i64) -> bool {
        self.flagged.lock().contains(&root_id)
    }

    /// Flags the root archive as a possible zip bomb (Autopsy
    /// `flagRootArchiveAsZipBomb`): an interesting-item result on the
    /// root and a warning naming the archive that triggered.
    pub(crate) fn flag_zip_bomb(
        &self,
        ctx: &JobContext,
        root: &Root,
        archive: &IngestFile,
        details: &str,
    ) {
        self.flagged.lock().insert(root.id);
        tracing::info!("{details}");
        Art::result(
            ctx,
            NAME,
            "TSK_INTERESTING_ITEM",
            root.id,
            Significance::LikelyNotable,
        )
        .configuration(ZIP_BOMB_SET)
        .text("TSK_SET_NAME", ZIP_BOMB_SET)
        .text(
            "TSK_DESCRIPTION",
            format!("Zip Bomb Detected {}", archive.rec.name),
        )
        .text("TSK_COMMENT", details)
        .post(ctx, &["TSK_SET_NAME", "TSK_DESCRIPTION", "TSK_COMMENT"]);
        ctx.post_message(
            IngestMessage::new(
                MessageKind::Warning,
                NAME,
                format!(
                    "Possible ZIP bomb detected in archive: {}, item: {}",
                    archive.rec.name,
                    escape_file_name(&archive.rec.path())
                ),
            )
            .details(details)
            .file(root.id),
        );
    }
}

impl FileModule for Worker {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        if file.is_unallocated_space() || file.known() == Known::Known || !file.is_file() {
            return Ok(());
        }
        match self.job.kind(file)? {
            Some(Kind::Archive(f)) => archive::unpack(&self.job, ctx, file, f),
            Some(Kind::Document(f)) => {
                // A document extracted from an archive starts a new nest.
                self.job.nesting.lock().remove(&file.rec.id);
                document::extract(&self.job, ctx, file, f)
            }
            None => Ok(()),
        }
    }
}

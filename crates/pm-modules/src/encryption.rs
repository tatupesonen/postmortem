//! Encryption Detection (Autopsy `modules/encryptiondetection`, inventory
//! 03 §2.7).
//!
//! File level (`EncryptionDetectionFileIngestModule`): a file typed
//! `application/octet-stream` whose size qualifies (≥ the minimum size and,
//! optionally, a multiple of 512 bytes; `.db` files from 64 KiB) and whose
//! byte entropy reaches the minimum gets `TSK_ENCRYPTION_SUSPECTED`
//! (Likely Notable, "Suspected encryption due to high entropy (%f).").
//! Otherwise a password-protected document — protected OOXML, or a
//! Word/Excel/PowerPoint, PDF or Access file that Tika/Jackcess cannot
//! open without a password — gets `TSK_ENCRYPTION_DETECTED` (Notable,
//! "Password protection detected."). Both carry the comment as
//! `TSK_COMMENT` and justification, and post an inbox data message.
//! Skipped: unallocated space, directories, slack (unless allowed), known
//! files, files with unallocated metadata and Autopsy's (misspelt)
//! `hiberfile.sys` / `pagefile.sys` in the root folder.
//!
//! Data-source level (`EncryptionDetectionDataSourceIngestModule`): for a
//! disk image, the first volume that is BitLocker-encrypted
//! (`TSK_ENCRYPTION_DETECTED`, "Bitlocker encryption detected.") or that
//! has no file system and high entropy (`TSK_ENCRYPTION_SUSPECTED`) is
//! flagged, and the check stops there, as in Autopsy. Beyond Autopsy,
//! LUKS volumes are detected and Core Storage (FileVault 2) volumes
//! suspected. Volume results are attached to the data source (the case
//! database has no volume objects) with the volume in `TSK_DESCRIPTION`.

use std::sync::Arc;

use pm_casedb::{DataSourceKind, Known, Origin, Significance};
use pm_core::io::SliceSource;
use pm_core::{ReadAt, SharedSource};
use pm_encryption::office::OfficeKind;
use pm_encryption::volume::{self, VolumeEncryption};
use pm_encryption::{access, entropy, office, pdf};
use pm_ingest::{
    DataSourceModule, DsProgress, FileModule, IngestFile, IngestMessage, JobContext, MessageKind,
    ModuleError, ModuleFactory, ModuleInfo, ModuleJob,
};
use serde::{Deserialize, Serialize};

use crate::file_type::{default_detector, mime_or_detect};
use crate::util::{Art, ds_result_exists, file_result_exists};

pub const NAME: &str = "Encryption Detection";
pub const PASSWORD_PROTECT_MESSAGE: &str = "Password protection detected.";
pub const BITLOCKER_MESSAGE: &str = "Bitlocker encryption detected.";
pub const LUKS_MESSAGE: &str = "LUKS encryption detected.";
pub const CORE_STORAGE_MESSAGE: &str =
    "Core Storage volume detected (FileVault 2 encryption suspected).";
const FILE_SIZE_MODULUS: u64 = 512;
const DATABASE_FILE_EXTENSION: &str = "db";
const MINIMUM_DATABASE_FILE_SIZE: u64 = 65_536;
/// Autopsy's ignore list (sic: `hiberfil.sys` is the real name).
const FILE_IGNORE_LIST: [&str; 2] = ["hiberfile.sys", "pagefile.sys"];
const OCTET_STREAM: &str = "application/octet-stream";
const MIME_TYPE_OOXML_PROTECTED: &str = "application/x-ooxml-protected";

/// `"Suspected encryption due to high entropy (%f)."`.
pub fn suspected_comment(entropy: f64) -> String {
    format!("Suspected encryption due to high entropy ({entropy:.6}).")
}

/// Per-job settings (`EncryptionDetectionIngestJobSettings`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncryptionSettings {
    /// "Minimum Entropy", 6.0–8.0.
    #[serde(default = "default_entropy")]
    pub minimum_entropy: f64,
    /// "Minimum File Size" in bytes (Autopsy's panel enters MB).
    #[serde(default = "default_size")]
    pub minimum_file_size: u64,
    /// "Consider only file sizes that are multiples of 512."
    #[serde(default = "yes")]
    pub file_size_multiple_enforced: bool,
    /// "Consider slack space files."
    #[serde(default = "yes")]
    pub slack_files_allowed: bool,
}

fn default_entropy() -> f64 {
    7.5
}

fn default_size() -> u64 {
    5_242_880
}

fn yes() -> bool {
    true
}

impl Default for EncryptionSettings {
    fn default() -> Self {
        Self {
            minimum_entropy: default_entropy(),
            minimum_file_size: default_size(),
            file_size_multiple_enforced: true,
            slack_files_allowed: true,
        }
    }
}

impl EncryptionSettings {
    /// `EncryptionDetectionTools.validateMinEntropyValue` /
    /// `validateMinFileSizeValue`.
    pub fn validate(&self) -> Result<(), ModuleError> {
        if !(6.0..=8.0).contains(&self.minimum_entropy) {
            return Err(ModuleError::new(
                "Minimum entropy input must be a number between 6.0 and 8.0.",
            ));
        }
        if self.minimum_file_size < 1 {
            return Err(ModuleError::new(
                "Minimum file size input must be an integer (in megabytes) of 1 or greater.",
            ));
        }
        Ok(())
    }

    /// `isFileEncryptionSuspected`'s size rule.
    pub fn size_qualifies(&self, extension: &str, size: u64) -> bool {
        if extension.eq_ignore_ascii_case(DATABASE_FILE_EXTENSION) {
            size >= MINIMUM_DATABASE_FILE_SIZE
        } else {
            size >= self.minimum_file_size
                && (!self.file_size_multiple_enforced || size.is_multiple_of(FILE_SIZE_MODULUS))
        }
    }
}

#[derive(Debug, Default)]
pub struct EncryptionFactory;

impl ModuleFactory for EncryptionFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "encryption",
            name: NAME,
            description: "Looks for files with the specified minimum entropy.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(EncryptionSettings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        _ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let settings: EncryptionSettings = if settings.is_null() {
            EncryptionSettings::default()
        } else {
            serde_json::from_value(settings.clone())
                .map_err(|e| ModuleError::new(format!("invalid settings: {e}")))?
        };
        settings.validate()?;
        Ok(Arc::new(Job(Arc::new(settings))))
    }
}

struct Job(Arc<EncryptionSettings>);

impl ModuleJob for Job {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(Worker(Arc::clone(&self.0))))
    }

    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        Some((1, Box::new(Volumes(Arc::clone(&self.0)))))
    }
}

/// What a file or volume is flagged with.
struct Flag {
    ty: &'static str,
    significance: Significance,
    comment: String,
    entropy: Option<f64>,
}

impl Flag {
    fn suspected(entropy: f64) -> Self {
        Self {
            ty: "TSK_ENCRYPTION_SUSPECTED",
            significance: Significance::LikelyNotable,
            comment: suspected_comment(entropy),
            entropy: Some(entropy),
        }
    }

    fn detected(comment: &str) -> Self {
        Self {
            ty: "TSK_ENCRYPTION_DETECTED",
            significance: Significance::Notable,
            comment: comment.to_owned(),
            entropy: None,
        }
    }

    fn display(&self) -> &'static str {
        if self.ty == "TSK_ENCRYPTION_DETECTED" {
            "Encryption Detected"
        } else {
            "Encryption Suspected"
        }
    }
}

/// Java's `Double.toString` for the entropy line of inbox messages.
fn java_double(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') || !v.is_finite() {
        s
    } else {
        format!("{s}.0")
    }
}

struct Worker(Arc<EncryptionSettings>);

impl FileModule for Worker {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        let s = &self.0;
        let rec = &file.rec;
        if matches!(rec.origin, Origin::UnallocBlocks | Origin::VirtualDir)
            || file.is_dir()
            || (rec.origin == Origin::Slack && !s.slack_files_allowed)
            || file.known() == Known::Known
            || !rec.meta_alloc
        {
            return Ok(());
        }
        if rec.parent_path == "/"
            && FILE_IGNORE_LIST
                .iter()
                .any(|n| rec.name.eq_ignore_ascii_case(n))
        {
            return Ok(());
        }
        let Some(mime) = mime_or_detect(file, default_detector()) else {
            return Ok(());
        };
        let src = file.content()?;
        let flag = if mime == OCTET_STREAM
            && let Some(e) = suspected(s, ctx, &file.rec.extension, &src)
        {
            Some(Flag::suspected(e))
        } else {
            password_protected(&mime, &*src).then(|| Flag::detected(PASSWORD_PROTECT_MESSAGE))
        };
        if let Some(flag) = flag
            && !ctx.is_cancelled()
        {
            let path = file.rec.path();
            post(ctx, Some(file.rec.id), &flag, &file.rec.name, &path, None);
        }
        Ok(())
    }
}

/// `isFileEncryptionSuspected`: the entropy when it reaches the minimum.
fn suspected(
    s: &EncryptionSettings,
    ctx: &JobContext,
    extension: &str,
    src: &SharedSource,
) -> Option<f64> {
    if !s.size_qualifies(extension, src.size()) {
        return None;
    }
    let e = entropy::entropy(&**src, &|| ctx.is_cancelled())?;
    (e >= s.minimum_entropy).then_some(e)
}

/// `isFilePasswordProtected`; parse failures are logged (Autopsy:
/// `ProcessResult.ERROR`) and not flagged.
fn password_protected(mime: &str, src: &dyn ReadAt) -> bool {
    let r = if mime == MIME_TYPE_OOXML_PROTECTED {
        Ok(true)
    } else if let Some(kind) = OfficeKind::from_mime(mime) {
        office::is_password_protected(src, kind)
    } else if mime == "application/pdf" {
        pdf::is_password_protected(src)
    } else if mime == "application/x-msaccess" {
        access::is_password_protected(src)
    } else {
        Ok(false)
    };
    r.unwrap_or_else(|e| {
        tracing::debug!("{NAME}: cannot check {mime} for a password: {e}");
        false
    })
}

/// `flagFile` / `flagVolume`: the result, and an inbox data message.
fn post(
    ctx: &JobContext,
    file_id: Option<i64>,
    flag: &Flag,
    name: &str,
    path: &str,
    volume: Option<String>,
) {
    let mut key: Vec<(&str, &str)> = vec![("TSK_COMMENT", &flag.comment)];
    if let Some(v) = &volume {
        key.push(("TSK_DESCRIPTION", v));
    }
    // Re-ingest posts nothing new (Autopsy would duplicate the result).
    let exists = match file_id {
        Some(id) => file_result_exists(ctx, id, flag.ty, &key),
        None => ds_result_exists(ctx, flag.ty, &key),
    };
    if exists {
        return;
    }
    let mut art = match file_id {
        Some(id) => Art::result(ctx, NAME, flag.ty, id, flag.significance),
        None => Art::ds_result(ctx, NAME, flag.ty, flag.significance),
    }
    .justification(flag.comment.clone())
    .text("TSK_COMMENT", flag.comment.clone());
    if let Some(v) = volume {
        art = art.text("TSK_DESCRIPTION", v);
    }
    art.post_unchecked(ctx);
    let details = match flag.entropy {
        Some(e) => format!("File: {path}\nEntropy: {}", java_double(e)),
        None => format!("File: {path}"),
    };
    let mut msg = IngestMessage::new(
        MessageKind::Data,
        NAME,
        format!("{} Match: {name}", flag.display()),
    )
    .details(details)
    .key(name);
    if let Some(id) = file_id {
        msg = msg.file(id);
    }
    ctx.post_message(msg);
}

/// The data-source module.
struct Volumes(Arc<EncryptionSettings>);

impl DataSourceModule for Volumes {
    fn process(&mut self, ctx: &JobContext, progress: &DsProgress<'_>) -> Result<(), ModuleError> {
        let db = ctx.case.db();
        let Some(ds) = db.data_source(ctx.ds_id)? else {
            return Ok(());
        };
        if !matches!(
            ds.kind,
            DataSourceKind::DiskImage | DataSourceKind::LocalDisk
        ) {
            return Ok(());
        }
        let volumes = db.volumes(ctx.ds_id)?;
        let file_systems = db.file_systems(ctx.ds_id)?;
        let image = ctx.case.image_source(ctx.ds_id)?;
        progress.message("Checking image for encryption.");
        progress.set_total(volumes.len() as u64);
        for (i, v) in volumes.iter().enumerate() {
            if ctx.is_cancelled() {
                return Ok(());
            }
            let sector = u64::from(v.sector_size);
            let start = v.start_sector.saturating_mul(sector);
            let len = v.sector_count.saturating_mul(sector);
            let vol = SliceSource::new(Arc::clone(&image), start, len);
            let head = vol
                .read_vec_at(0, volume::VOLUME_HEAD_LEN)
                .unwrap_or_default();
            let found = match volume::detect(&head) {
                Some(VolumeEncryption::BitLocker) => Some(Flag::detected(BITLOCKER_MESSAGE)),
                Some(VolumeEncryption::Luks) => Some(Flag::detected(LUKS_MESSAGE)),
                Some(VolumeEncryption::CoreStorage) => Some(Flag {
                    ty: "TSK_ENCRYPTION_SUSPECTED",
                    significance: Significance::LikelyNotable,
                    comment: CORE_STORAGE_MESSAGE.to_owned(),
                    entropy: None,
                }),
                None => {
                    // `isVolumeEncrypted`: no file system and high entropy.
                    let has_fs = file_systems.iter().any(|f| f.volume_id == Some(v.id));
                    if has_fs {
                        None
                    } else {
                        entropy::entropy(&vol, &|| ctx.is_cancelled())
                            .filter(|e| *e >= self.0.minimum_entropy)
                            .map(Flag::suspected)
                    }
                }
            };
            if let Some(flag) = found {
                if ctx.is_cancelled() {
                    return Ok(());
                }
                let name = v.description.clone();
                let label = format!(
                    "vol{} ({}) at sector {}",
                    v.addr, v.description, v.start_sector
                );
                post(
                    ctx,
                    None,
                    &flag,
                    &name,
                    &format!("{}/{name}", ds.name),
                    Some(label),
                );
                // Autopsy stops at the first flagged volume.
                return Ok(());
            }
            progress.set_done(i as u64 + 1);
        }
        Ok(())
    }
}

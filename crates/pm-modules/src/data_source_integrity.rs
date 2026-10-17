//! Data Source Integrity (Autopsy `modules/dataSourceIntegrity`,
//! inventory 03 §2.13), a stage-2 data-source module.
//!
//! For a disk image with stored hashes (from the E01 headers or entered
//! when it was added) it reads the whole image and verifies every stored
//! MD5 / SHA-1 / SHA-256; a mismatch posts `TSK_VERIFICATION_FAILED` on
//! the data source (Notable, comment and justification "<algorithm> hash
//! verification failed: …") and a warning message, a match an info
//! message. An image without stored hashes gets all three computed and
//! stored. Either step can be switched off; logical-file data sources are
//! skipped. Messages are Autopsy's, with the HTML turned into plain text.

use std::fmt::Write as _;
use std::sync::Arc;

use md5::{Digest, Md5};
use pm_casedb::integrity::ImageHash;
use pm_casedb::{DataSourceKind, Significance};
use pm_ingest::{
    DataSourceModule, DsProgress, IngestMessage, JobContext, MessageKind, ModuleError,
    ModuleFactory, ModuleInfo, ModuleJob,
};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::Sha256;

use crate::util::{Art, ds_result_exists};

pub const NAME: &str = "Data Source Integrity";
/// Bytes hashed per read (Autopsy reads 64 sectors at a time; the result
/// does not depend on it).
const CHUNK: usize = 4 << 20;

/// Per-job settings (`DataSourceIntegrityIngestSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegritySettings {
    /// "Calculate data source hashes if none are present".
    #[serde(default = "yes")]
    pub compute_hashes: bool,
    /// "Verify existing data source hashes".
    #[serde(default = "yes")]
    pub verify_hashes: bool,
}

fn yes() -> bool {
    true
}

impl Default for IntegritySettings {
    fn default() -> Self {
        Self {
            compute_hashes: true,
            verify_hashes: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct IntegrityFactory;

impl ModuleFactory for IntegrityFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "data_source_integrity",
            name: NAME,
            description: "Calculates and validates hashes of data sources.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(IntegritySettings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        _ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let s: IntegritySettings = if settings.is_null() {
            IntegritySettings::default()
        } else {
            serde_json::from_value(settings.clone())
                .map_err(|e| ModuleError::new(format!("invalid settings: {e}")))?
        };
        if !(s.compute_hashes || s.verify_hashes) {
            return Err(ModuleError::new(
                "At least one of the checkboxes must be selected",
            ));
        }
        Ok(Arc::new(Job(s)))
    }
}

struct Job(IntegritySettings);

impl ModuleJob for Job {
    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        Some((2, Box::new(Integrity(self.0))))
    }
}

struct Integrity(IntegritySettings);

/// The three digests, fed in one pass.
struct Hashers {
    md5: Md5,
    sha1: Sha1,
    sha256: Sha256,
}

impl Hashers {
    fn update(&mut self, data: &[u8], want: &[ImageHash]) {
        for a in want {
            match a {
                ImageHash::Md5 => self.md5.update(data),
                ImageHash::Sha1 => self.sha1.update(data),
                ImageHash::Sha256 => self.sha256.update(data),
            }
        }
    }

    /// Lower-case hex digests of `want`, in its order.
    fn finish(self, want: &[ImageHash]) -> Vec<(ImageHash, String)> {
        let hex = |b: &[u8]| pm_core::decode::to_hex(b);
        let md5 = hex(&self.md5.finalize());
        let sha1 = hex(&self.sha1.finalize());
        let sha256 = hex(&self.sha256.finalize());
        want.iter()
            .map(|&a| {
                let h = match a {
                    ImageHash::Md5 => md5.clone(),
                    ImageHash::Sha1 => sha1.clone(),
                    ImageHash::Sha256 => sha256.clone(),
                };
                (a, h)
            })
            .collect()
    }
}

impl DataSourceModule for Integrity {
    fn process(&mut self, ctx: &JobContext, progress: &DsProgress<'_>) -> Result<(), ModuleError> {
        let db = ctx.case.db();
        let Some(ds) = db.data_source(ctx.ds_id)? else {
            return Ok(());
        };
        let img = ds.name.clone();
        let info = |subject: String, details: String| {
            ctx.post_message(IngestMessage::new(MessageKind::Info, NAME, subject).details(details));
        };
        if !matches!(
            ds.kind,
            DataSourceKind::DiskImage | DataSourceKind::LocalDisk
        ) {
            info(
                format!("Skipping non-disk image data source {img}"),
                String::new(),
            );
            return Ok(());
        }
        let stored = db.data_source_hashes(ctx.ds_id)?;
        let verify = !stored.is_empty();
        if !verify && !self.0.compute_hashes {
            info(
                format!("Not computing new hashes for {img} since the option was disabled"),
                String::new(),
            );
            return Ok(());
        }
        if verify && !self.0.verify_hashes {
            info(
                format!("Not verifying existing hashes for {img} since the option was disabled"),
                String::new(),
            );
            return Ok(());
        }
        let want: Vec<ImageHash> = if verify {
            stored.iter().map(|(a, _)| *a).collect()
        } else {
            ImageHash::ALL.to_vec()
        };
        info(format!("Starting {img}"), String::new());
        let src = ctx.case.image_source(ctx.ds_id)?;
        let size = src.size();
        progress.message(format!("Hashing {img}"));
        progress.set_total(size.div_ceil(CHUNK as u64));
        let mut hashers = Hashers {
            md5: Md5::new(),
            sha1: Sha1::new(),
            sha256: Sha256::new(),
        };
        let mut buf = vec![0u8; CHUNK];
        let mut off = 0u64;
        let mut chunk = 0u64;
        while off < size {
            if ctx.is_cancelled() {
                return Ok(());
            }
            let n = usize::try_from((size - off).min(CHUNK as u64)).unwrap_or(CHUNK);
            let part = buf.get_mut(..n).unwrap_or_default();
            if let Err(e) = src.read_exact_at(off, part) {
                let msg = format!("Error reading {img} at chunk {chunk}");
                tracing::error!("{msg}: {e}");
                ctx.post_message(IngestMessage::new(MessageKind::Error, NAME, msg));
                return Err(ModuleError::new(e.to_string()));
            }
            hashers.update(part, &want);
            off += n as u64;
            chunk += 1;
            progress.set_done(chunk);
        }
        let calculated = hashers.finish(&want);
        if verify {
            verify_hashes(ctx, &img, &stored, &calculated);
        } else {
            let mut results = format!("Data Source Hash Calculation Results for {img}");
            for (a, h) in &calculated {
                if let Err(e) = db.set_data_source_hash(ctx.ds_id, *a, h) {
                    tracing::error!("Error setting calculated hash: {e}");
                }
                let _ = write!(results, "\nCalculated {} hash: {h}", a.name());
            }
            info(format!("{img} hashes calculated"), results);
        }
        Ok(())
    }
}

/// The verification report and, on a mismatch, `TSK_VERIFICATION_FAILED`.
fn verify_hashes(
    ctx: &JobContext,
    img: &str,
    stored: &[(ImageHash, String)],
    calculated: &[(ImageHash, String)],
) {
    let mut verified = true;
    let mut hash_results = String::new();
    let mut comment = String::new();
    for ((algo, stored), (_, calc)) in stored.iter().zip(calculated) {
        // Autopsy compares case-sensitively against a lower-case digest;
        // stored values are compared ignoring case here.
        if stored.eq_ignore_ascii_case(calc) {
            let _ = write!(hash_results, "\n{} hash verified", algo.name());
        } else {
            verified = false;
            let _ = write!(hash_results, "\n{} hash not verified", algo.name());
            let _ = write!(
                comment,
                "{} hash verification failed:\n  Calculated hash: {calc}\n  Stored hash: {stored}\n ",
                algo.name()
            );
        }
        let _ = write!(
            hash_results,
            "\n  Calculated hash: {calc}\n  Stored hash: {stored}"
        );
    }
    let (kind, subject, result) = if verified {
        (
            MessageKind::Info,
            format!("Integrity of {img} verified"),
            " verified ",
        )
    } else {
        (
            MessageKind::Warning,
            format!("{img} failed integrity verification"),
            " not verified ",
        )
    };
    let details =
        format!("Data Source Verification Results for {img}\nResult:{result}{hash_results}");
    if !verified && !ds_result_exists(ctx, "TSK_VERIFICATION_FAILED", &[("TSK_COMMENT", &comment)])
    {
        Art::ds_result(ctx, NAME, "TSK_VERIFICATION_FAILED", Significance::Notable)
            .justification(comment.clone())
            .text("TSK_COMMENT", comment)
            .post_unchecked(ctx);
    }
    ctx.post_message(IngestMessage::new(kind, NAME, subject).details(details));
}

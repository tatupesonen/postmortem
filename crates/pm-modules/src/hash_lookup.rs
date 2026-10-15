//! Hash Lookup (Autopsy `modules/hashdatabase`, inventory 03 §2.1).
//!
//! Computes MD5 and SHA-256 in one pass and checks the MD5 against the
//! configured hash sets:
//! * **notable** sets: the file becomes notable and gets a
//!   `TSK_HASHSET_HIT` result (score Notable);
//! * **no-change** sets: a hit is recorded, the file's status is untouched;
//! * **known** sets (only when nothing notable matched): the file becomes
//!   known; no artifact.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use pm_casedb::{Known, Significance};
use pm_hashdb::{HashDb, HashDbError};
use pm_ingest::{
    FileModule, IngestFile, IngestMessage, JobContext, MessageKind, ModuleError, ModuleFactory,
    ModuleInfo, ModuleJob,
};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::Sha256;

use crate::util::{Art, hex};

pub const NAME: &str = "Hash Lookup";
/// Comments longer than this are truncated (Autopsy).
const MAX_COMMENT: usize = 500;
const READ_CHUNK: usize = 1 << 20;

/// What a hash set says about its members (Autopsy `KnownFilesType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetKind {
    Known,
    Notable,
    NoChange,
}

/// A hash set used by a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashSetConfig {
    pub path: PathBuf,
    /// Display name (default: the set's own name).
    #[serde(default)]
    pub name: Option<String>,
    pub kind: SetKind,
    #[serde(default = "yes")]
    pub search_during_ingest: bool,
    /// Post an inbox message per hit (default: on for notable sets).
    #[serde(default)]
    pub send_ingest_messages: Option<bool>,
}

fn yes() -> bool {
    true
}

/// Per-job settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashLookupSettings {
    /// "Calculate MD5 even if no hash set is selected".
    #[serde(default = "yes")]
    pub calculate_hashes: bool,
    #[serde(default)]
    pub sets: Vec<HashSetConfig>,
}

impl Default for HashLookupSettings {
    fn default() -> Self {
        Self {
            calculate_hashes: true,
            sets: Vec::new(),
        }
    }
}

#[derive(Debug, Default)]
pub struct HashLookupFactory;

impl ModuleFactory for HashLookupFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "hash_lookup",
            name: NAME,
            description: "Identifies known and notable files using supplied hash sets, such as a standard NSRL hash set.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(HashLookupSettings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let settings: HashLookupSettings = serde_json::from_value(settings.clone())
            .map_err(|e| ModuleError::new(format!("invalid settings: {e}")))?;
        let mut sets = Vec::new();
        for cfg in settings.sets.iter().filter(|s| s.search_during_ingest) {
            match open_set(&cfg.path) {
                Ok(db) => {
                    let name = cfg.name.clone().unwrap_or_else(|| db.display_name());
                    let messages = cfg
                        .send_ingest_messages
                        .unwrap_or(cfg.kind != SetKind::Known);
                    sets.push(Set {
                        db,
                        name,
                        kind: cfg.kind,
                        messages,
                    });
                }
                Err(e) => ctx.post_message(
                    IngestMessage::new(
                        MessageKind::Error,
                        NAME,
                        format!("Could not open hash set {}", cfg.path.display()),
                    )
                    .details(e.to_string()),
                ),
            }
        }
        let has = |k: SetKind| sets.iter().any(|s| s.kind == k);
        if !has(SetKind::Notable) {
            ctx.post_message(IngestMessage::new(
                MessageKind::Warning,
                NAME,
                "No notable hash set.",
            ));
        }
        if !has(SetKind::Known) {
            ctx.post_message(IngestMessage::new(
                MessageKind::Warning,
                NAME,
                "No known hash set.",
            ));
        }
        Ok(Arc::new(JobRef(Arc::new(Job {
            calculate: settings.calculate_hashes,
            sets,
            stats: Stats::default(),
        }))))
    }
}

fn open_set(path: &std::path::Path) -> Result<HashDb, HashDbError> {
    // Text sets are indexed on first use (Autopsy requires a manual
    // "Index" step; doing it here keeps the job from failing).
    HashDb::open_or_index(path, &AtomicU64::new(0))
}

struct Set {
    db: HashDb,
    name: String,
    kind: SetKind,
    messages: bool,
}

#[derive(Default)]
struct Stats {
    notable: AtomicU64,
    no_change: AtomicU64,
    hash_ns: AtomicU64,
    lookup_ns: AtomicU64,
}

struct Job {
    calculate: bool,
    sets: Vec<Set>,
    stats: Stats,
}

impl Job {
    /// Summary message (Autopsy posts it from the last instance).
    fn finish(&self, ctx: &JobContext) {
        if self.sets.is_empty() {
            return;
        }
        let ms = |a: &AtomicU64| Duration::from_nanos(a.load(Ordering::Relaxed)).as_millis();
        let list = |k: SetKind| {
            self.sets
                .iter()
                .filter(|s| s.kind == k)
                .map(|s| format!("  {}", s.name))
                .collect::<Vec<_>>()
                .join("\n")
        };
        ctx.post_message(
            IngestMessage::new(MessageKind::Info, NAME, "Hash Lookup Results").details(format!(
                "Notables found: {}\nTotal Calculation Time: {} ms\nTotal Lookup Time: {} ms\n\
                 Hash Sets Used:\n{}{}",
                self.stats.notable.load(Ordering::Relaxed),
                ms(&self.stats.hash_ns),
                ms(&self.stats.lookup_ns),
                list(SetKind::Notable),
                {
                    let nc = list(SetKind::NoChange);
                    if nc.is_empty() {
                        String::new()
                    } else {
                        format!("\n{nc}")
                    }
                },
            )),
        );
    }
}

/// `Arc` wrapper so worker instances share the job's sets.
struct JobRef(Arc<Job>);

impl ModuleJob for JobRef {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(Worker {
            job: Arc::clone(&self.0),
            buf: vec![0; READ_CHUNK],
        }))
    }

    fn finish(&self, ctx: &JobContext) {
        self.0.finish(ctx);
    }
}

struct Worker {
    job: Arc<Job>,
    buf: Vec<u8>,
}

impl FileModule for Worker {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        // Autopsy skips only unallocated blocks, slack and directories:
        // virtual files ($MBR, $FAT1), symlinks, devices and deleted names
        // without metadata are hashed too.
        if file.is_unallocated_space() || file.is_dir() {
            return Ok(());
        }
        let job = Arc::clone(&self.job);
        let any_lookup = job
            .sets
            .iter()
            .any(|s| matches!(s.kind, SetKind::Known | SetKind::Notable));
        if !any_lookup && !job.calculate {
            return Ok(());
        }
        // Reuse hashes computed earlier only when all of them are present.
        let existing = file
            .sha256()
            .and(file.sha1())
            .and(file.md5())
            .and_then(to16);
        let md5 = if let Some(m) = existing {
            m
        } else {
            let t = Instant::now();
            let r = self.hash(ctx, file);
            job.stats
                .hash_ns
                .fetch_add(nanos(t.elapsed()), Ordering::Relaxed);
            match r {
                Ok(Some(m)) => m,
                Ok(None) => return Ok(()), // cancelled mid-file
                Err(e) => {
                    // Autopsy reports the read error and carries on (there is
                    // no hash to look up).
                    let state = if file.rec.meta_alloc {
                        "Allocated File"
                    } else {
                        "Deleted File"
                    };
                    tracing::warn!(file = %file.rec.path(), "error calculating hash: {e}");
                    ctx.post_message(
                        IngestMessage::new(
                            MessageKind::Error,
                            NAME,
                            format!("Read Error: {}", file.rec.name),
                        )
                        .details(format!(
                            "Error encountered while calculating the hash value for {} ({state}).",
                            file.rec.path()
                        ))
                        .file(file.rec.id),
                    );
                    return Ok(());
                }
            }
        };

        let t = Instant::now();
        let result = self.lookup(ctx, file, &md5);
        job.stats
            .lookup_ns
            .fetch_add(nanos(t.elapsed()), Ordering::Relaxed);
        result
    }
}

impl Worker {
    /// MD5, SHA-1 and SHA-256 in one pass (Autopsy computes MD5 and
    /// SHA-256; postmortem adds SHA-1); `None` if the job was cancelled.
    fn hash(
        &mut self,
        ctx: &JobContext,
        file: &mut IngestFile,
    ) -> Result<Option<[u8; 16]>, ModuleError> {
        let src = file.content()?;
        let size = src.size();
        let mut md5 = Md5::new();
        let mut sha1 = Sha1::new();
        let mut sha = Sha256::new();
        let mut off = 0u64;
        while off < size {
            if ctx.is_cancelled() {
                return Ok(None);
            }
            let want = usize::try_from((size - off).min(self.buf.len() as u64)).unwrap_or(0);
            let buf = self.buf.get_mut(..want).unwrap_or_default();
            let n = src.read_at(off, buf)?;
            if n == 0 {
                return Err(ModuleError::new(format!(
                    "unexpected end of content at byte {off}"
                )));
            }
            let chunk = buf.get(..n).unwrap_or_default();
            md5.update(chunk);
            sha1.update(chunk);
            sha.update(chunk);
            off += n as u64;
        }
        let m: [u8; 16] = md5.finalize().into();
        file.set_md5(m.to_vec());
        file.set_sha1(sha1.finalize().to_vec());
        file.set_sha256(sha.finalize().to_vec());
        Ok(Some(m))
    }

    fn lookup(
        &self,
        ctx: &JobContext,
        file: &mut IngestFile,
        md5: &[u8; 16],
    ) -> Result<(), ModuleError> {
        let job = &*self.job;
        let mut notable_hit = false;
        let mut errors = Vec::new();
        for set in job
            .sets
            .iter()
            .filter(|s| matches!(s.kind, SetKind::Notable | SetKind::NoChange))
        {
            match set.db.lookup(md5) {
                Ok(Some(hit)) => {
                    let notable = set.kind == SetKind::Notable;
                    if notable {
                        notable_hit = true;
                        file.set_known(Known::Notable);
                        job.stats.notable.fetch_add(1, Ordering::Relaxed);
                    } else {
                        job.stats.no_change.fetch_add(1, Ordering::Relaxed);
                    }
                    let comment = merge_comments(&hit.comments);
                    let significance = if notable {
                        Significance::Notable
                    } else {
                        Significance::Unknown
                    };
                    Art::result(ctx, NAME, "TSK_HASHSET_HIT", file.rec.id, significance)
                        .configuration(&set.name)
                        .text("TSK_SET_NAME", &set.name)
                        .text("TSK_HASH_MD5", hex(md5))
                        .text("TSK_COMMENT", comment)
                        .post(ctx, &["TSK_SET_NAME"]);
                    if set.messages {
                        let label = if notable { "Notable" } else { "No Change" };
                        ctx.post_message(
                            IngestMessage::new(
                                MessageKind::Data,
                                NAME,
                                format!("{label}: {}", file.rec.name),
                            )
                            .details(format!(
                                "File Name: {}\nMD5 Hash: {}\nHash Set Name: {}",
                                file.rec.path(),
                                hex(md5),
                                set.name
                            ))
                            .key(format!("{}{}", file.rec.name, hex(md5)))
                            .file(file.rec.id),
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => errors.push((set.kind, format!("{}: {e}", set.name))),
            }
        }
        if !notable_hit {
            for set in job.sets.iter().filter(|s| s.kind == SetKind::Known) {
                match set.db.contains(md5) {
                    Ok(true) => {
                        file.set_known(Known::Known);
                        break;
                    }
                    Ok(false) => {}
                    Err(e) => errors.push((SetKind::Known, format!("{}: {e}", set.name))),
                }
            }
        }
        if errors.is_empty() {
            return Ok(());
        }
        for (kind, cause) in &errors {
            let what = match kind {
                SetKind::Notable => "notable",
                SetKind::NoChange => "no change",
                SetKind::Known => "known",
            };
            tracing::warn!(file = %file.rec.path(), "hash set lookup failed: {cause}");
            ctx.post_message(
                IngestMessage::new(
                    MessageKind::Error,
                    NAME,
                    format!("Hash Lookup Error: {}", file.rec.name),
                )
                .details(format!(
                    "Error encountered while looking up {what} hash value for {}.",
                    file.rec.name
                ))
                .file(file.rec.id),
            );
        }
        let causes: Vec<&str> = errors.iter().map(|(_, c)| c.as_str()).collect();
        Err(ModuleError::new(causes.join("; ")))
    }
}

/// Space-joined comments, truncated to 500 characters + "..." (Autopsy).
pub fn merge_comments(comments: &[String]) -> String {
    let joined = comments.join(" ");
    if joined.chars().count() > MAX_COMMENT {
        let cut: String = joined.chars().take(MAX_COMMENT).collect();
        format!("{cut}...")
    } else {
        joined
    }
}

fn to16(b: &[u8]) -> Option<[u8; 16]> {
    b.try_into().ok()
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

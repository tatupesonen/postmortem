//! What modules see: the job context and the file being analysed.

use std::sync::{Arc, OnceLock};

use crossbeam_channel::Sender;
use pm_case::Case;
use pm_casedb::{FileAnalysis, FileKindCode, FileRecord, Known, NewArtifact, Origin};
use pm_core::SharedSource;

use crate::{Inbox, IngestMessage, IngestProgress, ModuleError};

/// Bytes of each file cached for signature checks.
pub const HEAD_LEN: usize = 8192;

/// Database writes produced by modules, applied by the job's writer thread.
#[derive(Debug)]
pub(crate) enum WriteOp {
    Analysis(i64, FileAnalysis),
    /// Artifact plus the attribute types that identify a duplicate
    /// (`None`: insert without a duplicate check).
    Artifact(NewArtifact, Option<Vec<i64>>),
    /// End of the job: the writer commits and exits.
    Stop,
}

/// Job-wide context handed to every module call.
#[derive(Debug, Clone)]
pub struct JobContext {
    pub ds_id: i64,
    /// Case database id of the ingest job record.
    pub job_id: i64,
    pub case: Arc<Case>,
    /// Whether unallocated space is analysed (from the file filter).
    pub process_unallocated: bool,
    pub(crate) sink: Sender<WriteOp>,
    pub(crate) inbox: Arc<Inbox>,
    pub(crate) progress: Arc<IngestProgress>,
}

impl JobContext {
    /// True once the job has been cancelled; long loops should poll this.
    pub fn is_cancelled(&self) -> bool {
        self.progress.is_cancelled()
    }

    /// Cancels the whole job (e.g. a module detects the case volume is
    /// full).
    pub fn cancel_job(&self) {
        self.progress.cancel();
    }

    /// Posts an artifact or analysis result unless the file already has one
    /// of the same type whose attributes include this one's `key`
    /// attributes (re-ingest is idempotent).
    pub fn post_artifact(&self, artifact: NewArtifact, key: &[i64]) {
        // The writer only disappears when the job is failing; the error
        // is reported there.
        let _ = self.sink.send(WriteOp::Artifact(artifact, Some(key.to_vec())));
    }

    /// Posts an artifact without the duplicate check (for bulk extractors
    /// that make re-runs idempotent themselves, e.g. by skipping files
    /// they have already processed).
    pub fn post_artifact_unchecked(&self, artifact: NewArtifact) {
        let _ = self.sink.send(WriteOp::Artifact(artifact, None));
    }

    /// Posts an ingest inbox message.
    pub fn post_message(&self, msg: IngestMessage) {
        self.inbox.post(msg);
    }

    /// Current time as Unix seconds (artifact `created_at`).
    pub fn now(&self) -> i64 {
        jiff::Timestamp::now().as_second()
    }
}

/// A file passing through the pipeline. Modules read it and record their
/// findings on it; the framework persists the changes after the last module.
#[derive(Debug)]
pub struct IngestFile {
    pub rec: FileRecord,
    case: Arc<Case>,
    content: OnceLock<Result<SharedSource, ModuleError>>,
    head: OnceLock<Vec<u8>>,
    update: FileAnalysis,
    dirty: bool,
}

impl IngestFile {
    pub fn new(rec: FileRecord, case: Arc<Case>) -> Self {
        Self {
            rec,
            case,
            content: OnceLock::new(),
            head: OnceLock::new(),
            update: FileAnalysis::default(),
            dirty: false,
        }
    }

    /// A regular file with content (Autopsy's `isFile()`), including
    /// alternate data streams, unallocated-space and carved files.
    pub fn is_file(&self) -> bool {
        self.rec.kind == FileKindCode::Regular
    }

    pub fn is_dir(&self) -> bool {
        self.rec.kind == FileKindCode::Directory
    }

    /// Unallocated blocks or slack (skipped by most file modules).
    pub fn is_unallocated_space(&self) -> bool {
        matches!(self.rec.origin, Origin::UnallocBlocks | Origin::Slack)
    }

    /// Name or metadata is unallocated.
    pub fn is_deleted(&self) -> bool {
        self.rec.deleted()
    }

    /// The file's content (opened once and shared by all modules).
    pub fn content(&self) -> Result<SharedSource, ModuleError> {
        self.content
            .get_or_init(|| self.case.content(&self.rec).map_err(ModuleError::from))
            .clone()
    }

    /// The first [`HEAD_LEN`] bytes (fewer for small files; empty if the
    /// content cannot be read).
    pub fn head(&self) -> &[u8] {
        self.head.get_or_init(|| {
            let Ok(src) = self.content() else {
                return Vec::new();
            };
            let n = usize::try_from(src.size().min(HEAD_LEN as u64)).unwrap_or(0);
            let mut buf = vec![0; n];
            let got = src.read_at(0, &mut buf).unwrap_or(0);
            buf.truncate(got);
            buf
        })
    }

    /// MIME type (set during this job or earlier).
    pub fn mime(&self) -> Option<&str> {
        self.update.mime.as_deref().or(self.rec.mime.as_deref())
    }

    pub fn set_mime(&mut self, mime: impl Into<String>) {
        self.update.mime = Some(mime.into());
        self.dirty = true;
    }

    pub fn md5(&self) -> Option<&[u8]> {
        self.update.md5.as_deref().or(self.rec.md5.as_deref())
    }

    pub fn sha1(&self) -> Option<&[u8]> {
        self.update.sha1.as_deref().or(self.rec.sha1.as_deref())
    }

    pub fn sha256(&self) -> Option<&[u8]> {
        self.update.sha256.as_deref().or(self.rec.sha256.as_deref())
    }

    pub fn set_md5(&mut self, v: Vec<u8>) {
        self.update.md5 = Some(v);
        self.dirty = true;
    }

    pub fn set_sha1(&mut self, v: Vec<u8>) {
        self.update.sha1 = Some(v);
        self.dirty = true;
    }

    pub fn set_sha256(&mut self, v: Vec<u8>) {
        self.update.sha256 = Some(v);
        self.dirty = true;
    }

    pub fn known(&self) -> Known {
        self.update.known.unwrap_or(self.rec.known)
    }

    pub fn set_known(&mut self, k: Known) {
        self.update.known = Some(k);
        self.dirty = true;
    }

    /// Changes to persist, if any.
    pub(crate) fn take_update(&mut self) -> Option<FileAnalysis> {
        self.dirty
            .then(|| std::mem::take(&mut self.update))
            .inspect(|_| self.dirty = false)
    }
}

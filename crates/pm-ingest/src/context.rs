//! What modules see: the job context and the file being analysed.

use std::ops::Range;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use crossbeam_channel::{Sender, bounded};
use pm_case::Case;
use pm_casedb::{FileAnalysis, FileKindCode, FileRecord, Known, NewArtifact, NewFile, Origin};
use pm_core::SharedSource;

use crate::sched::Scheduler;
use crate::{FileFilter, Inbox, IngestMessage, IngestProgress, ModuleError};

/// Bytes of each file cached for signature checks.
pub const HEAD_LEN: usize = 8192;

/// Where the writer reports the records of inserted derived files.
pub(crate) type FilesReply = Sender<Result<Vec<FileRecord>, String>>;

/// Database writes produced by modules, applied by the job's writer thread.
#[derive(Debug)]
pub(crate) enum WriteOp {
    Analysis(i64, FileAnalysis),
    /// Artifact plus the attribute types that identify a duplicate
    /// (`None`: insert without a duplicate check).
    Artifact(NewArtifact, Option<Vec<i64>>),
    /// New derived/carved files; the writer commits, then replies with
    /// their records.
    Files(Vec<NewFile>, FilesReply),
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
    pub(crate) sched: Arc<Scheduler>,
    pub(crate) filter: FileFilter,
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
        let _ = self
            .sink
            .send(WriteOp::Artifact(artifact, Some(key.to_vec())));
    }

    /// Posts an artifact without the duplicate check (for bulk extractors
    /// that make re-runs idempotent themselves, e.g. by skipping files
    /// they have already processed).
    pub fn post_artifact_unchecked(&self, artifact: NewArtifact) {
        let _ = self.sink.send(WriteOp::Artifact(artifact, None));
    }

    /// Reserves `n` consecutive file ids, so a tree of derived files can
    /// reference its parents before the rows are written.
    pub fn reserve_file_ids(&self, n: u64) -> Range<i64> {
        self.case.db().file_ids().reserve(n)
    }

    /// Adds derived, carved or local files (Autopsy `addDerivedFile` +
    /// `addFilesToJob`): the job's writer inserts them (parents before
    /// children) and commits, then they are scheduled ahead of all other
    /// files so they run through the whole file pipeline, recursively.
    /// Rows without an id get one from the shared allocator.
    pub fn add_derived_files(&self, files: Vec<NewFile>) -> Result<Vec<FileRecord>, ModuleError> {
        let records = self.insert_derived_files(files)?;
        self.add_files_to_job(records.clone());
        Ok(records)
    }

    /// Inserts derived, carved or local files through the job's writer
    /// (committed when this returns) **without** scheduling them; the
    /// module calls [`Self::add_files_to_job`] once it has recorded what
    /// it needs about them (e.g. the nesting depth of extracted archives,
    /// which another worker may otherwise look up too early).
    pub fn insert_derived_files(
        &self,
        files: Vec<NewFile>,
    ) -> Result<Vec<FileRecord>, ModuleError> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let (reply, answer) = bounded(1);
        self.sink
            .send(WriteOp::Files(files, reply))
            .map_err(|_| ModuleError::new("the result writer has stopped"))?;
        answer
            .recv()
            .map_err(|_| ModuleError::new("the result writer has stopped"))?
            .map_err(ModuleError)
    }

    /// Schedules files that are already in the case database as
    /// high-priority tasks of this job (Autopsy `addFilesToJob`). Files
    /// the job's file filter rejects are skipped; nothing is scheduled
    /// once the job is cancelled.
    pub fn add_files_to_job(&self, files: Vec<FileRecord>) {
        if self.is_cancelled() {
            return;
        }
        let files: Vec<FileRecord> = files
            .into_iter()
            .filter(|f| self.filter.accepts(f))
            .collect();
        self.progress
            .files_total
            .fetch_add(files.len() as u64, Ordering::Relaxed);
        self.sched.push_high(files);
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

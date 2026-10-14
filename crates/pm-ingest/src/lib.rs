//! The postmortem ingest framework (ADR-015).
//!
//! Analysers ("ingest modules") implement [`ModuleFactory`] and the module
//! traits; [`run_job`] runs the enabled modules over a data source:
//!
//! * files stream from the case database in Autopsy's root-priority order
//!   (user folders first, page files and unallocated space last);
//! * `N` workers each own one instance of every file module, called in
//!   pipeline order; content is opened once per file and shared;
//! * MIME types, hashes, known status and artifacts are saved by one writer
//!   thread in short transactions; artifacts are de-duplicated;
//! * data-source modules run in numbered stages;
//! * [`IngestProgress`] exposes what every worker is doing, for the status
//!   bar and the progress snapshot.

mod context;
mod inbox;
mod job;
mod module;
mod progress;

pub use context::{HEAD_LEN, IngestFile, JobContext};
pub use inbox::{Inbox, IngestMessage, MAX_ERROR_MESSAGES, MessageKind};
pub use job::{FileFilter, IngestError, JobSettings, JobSummary, ModuleSetting, run_job};
pub use module::{DataSourceModule, FileModule, ModuleError, ModuleFactory, ModuleInfo, ModuleJob};
pub use progress::{Activity, DsProgress, DsStatus, IngestProgress, Phase, Snapshot};

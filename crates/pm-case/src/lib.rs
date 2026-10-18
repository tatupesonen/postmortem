//! postmortem cases.
//!
//! A case is a folder (Autopsy's case directory) holding:
//!
//! ```text
//! <case>/
//!   case.pmcase      case metadata (TOML) — the file users open
//!   case.db          case database (pm-casedb)
//!   Export/  Reports/  ModuleOutput/  Temp/  Log/
//! ```
//!
//! [`Case`] owns the database and lazily re-opens evidence when file content
//! is needed; [`load`] adds disk images to a case with a parallel walker and a
//! single database writer.

mod case;
mod content;
pub mod derived;
mod l01;
pub mod load;
pub mod logical;
mod logical_imager;
pub mod raw_sources;
pub mod spec;
mod timezone;
pub use timezone::ZoneMismatch;
mod xry;

pub use case::{
    CASE_FILE, Case, CaseDetails, CaseKind, DB_FILE, Examiner, SUBDIRS, delete_case_folder,
};
pub use content::{layout_json, parse_layout};
pub use load::{LoadOptions, LoadProgress, LoadSummary, add_evidence, add_image};
pub use logical::{LogicalOptions, add_logical_files};
pub use logical_imager::{add_logical_imager, is_logical_imager_folder};
pub use raw_sources::{
    LocalDisk, RAW_CHUNK, RawOptions, add_local_disk, add_unallocated_image, local_disks,
};
pub use spec::{DataSourceSpec, add_data_source};
pub use xry::{XRY_SOURCE_NAME, add_xry_folder, is_xry_file, xry_files};

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CaseError {
    #[error(transparent)]
    Db(#[from] pm_casedb::DbError),
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid case file {path}: {reason}")]
    BadCaseFile { path: PathBuf, reason: String },
    #[error("case folder {0} already exists and is not empty")]
    FolderNotEmpty(PathBuf),
    #[error("evidence for data source {ds_id} not found at {path}")]
    MissingEvidence { ds_id: i64, path: PathBuf },
    #[error(transparent)]
    Evidence(#[from] pm_evidence::EvidenceError),
    #[error(transparent)]
    Volume(#[from] pm_volume::VolumeError),
    #[error(transparent)]
    Fs(#[from] pm_fs::FsError),
    #[error("{0} not found")]
    NotFound(String),
    #[error("cancelled")]
    Cancelled,
    /// Evidence that is not what the data source type expects.
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Search(#[from] pm_search::SearchError),
}

pub type Result<T, E = CaseError> = std::result::Result<T, E>;

pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> CaseError {
    let path = path.into();
    move |source| CaseError::Io { path, source }
}

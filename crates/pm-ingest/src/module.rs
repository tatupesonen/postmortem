//! The module contract (Autopsy's `IngestModuleFactory` / `IngestModule`).

use std::sync::Arc;

use crate::{DsProgress, IngestFile, JobContext};

/// Identity and description of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleInfo {
    /// Stable id used in settings and job records (`"hash_lookup"`).
    pub id: &'static str,
    /// Display name (Autopsy's identity key, `"Hash Lookup"`).
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,
    /// Enabled when the job settings do not mention the module.
    pub enabled_by_default: bool,
}

/// A failure reported by a module; the pipeline logs it and continues.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ModuleError(pub String);

impl ModuleError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl From<std::io::Error> for ModuleError {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}

impl From<pm_case::CaseError> for ModuleError {
    fn from(e: pm_case::CaseError) -> Self {
        Self(e.to_string())
    }
}

impl From<pm_casedb::DbError> for ModuleError {
    fn from(e: pm_casedb::DbError) -> Self {
        Self(e.to_string())
    }
}

/// Creates per-job module state.
pub trait ModuleFactory: Send + Sync {
    fn info(&self) -> ModuleInfo;

    /// Per-job settings used when the job does not provide any.
    fn default_settings(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Starts the module for a job. An error aborts the job before any data
    /// is processed (Autopsy's `startUp` failure).
    fn start(
        &self,
        ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError>;
}

/// A module's state for one job, shared by all of its instances.
pub trait ModuleJob: Send + Sync {
    /// A file-module instance for one worker thread (`None`: the module has
    /// no file-level work). Each instance is only called from one thread.
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        None
    }

    /// A data-source module and the stage it runs in. Stage 1 runs alongside
    /// file analysis; later stages start once all earlier work has finished.
    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        None
    }

    /// Called once, after every instance has shut down (summary messages).
    fn finish(&self, _ctx: &JobContext) {}
}

/// Per-file analysis.
pub trait FileModule: Send {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError>;

    /// Called once when the worker finishes (also after cancellation).
    fn shut_down(&mut self, _ctx: &JobContext) {}
}

/// Whole-data-source analysis.
pub trait DataSourceModule: Send {
    fn process(&mut self, ctx: &JobContext, progress: &DsProgress<'_>) -> Result<(), ModuleError>;
}

//! WebAssembly plugin host (ADR-022): loads component plugins built for the
//! `postmortem:plugin` WIT world (`pm-plugin-api/wit/plugin.wit`) and runs
//! them as [`pm_plugin_api::Plugin`]s, so the ingest framework treats them
//! exactly like built-in modules.
//!
//! Sandbox: a plugin sees only the host interface (file content, results,
//! derived files, log, messages). WASI is linked for the Rust standard
//! library but with no preopened directories, no environment, no
//! arguments, no network and no inherited stdio. Each call gets a fuel
//! budget and each instance a memory cap; a trap or an exhausted budget
//! fails that call only, and the worker's instance is recreated.
//!
//! Plugins are folders with a `plugin.toml` manifest next to the
//! component (see [`manifest`]); [`load_dir`] loads every folder below a
//! plugins directory.

mod convert;
pub mod manifest;
mod runtime;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use manifest::Manifest;
pub use runtime::{Limits, WasmPlugin};

/// A plugin that could not be loaded, with the reason (shown in the
/// plugin manager).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {reason}")]
pub struct LoadError {
    pub path: PathBuf,
    pub reason: String,
}

/// The shared compilation engine (one per process: it owns the code cache
/// and compiler threads).
pub fn engine() -> &'static wasmtime::Engine {
    runtime::engine()
}

/// Loads every plugin folder (a folder with a `plugin.toml`) directly below
/// `dir`, in name order. A missing `dir` is an empty list.
pub fn load_dir(dir: &Path) -> Vec<Result<Arc<WasmPlugin>, LoadError>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(manifest::FILE_NAME).is_file())
        .collect();
    folders.sort();
    folders
        .into_iter()
        .map(|f| WasmPlugin::load(&f).map(Arc::new))
        .collect()
}

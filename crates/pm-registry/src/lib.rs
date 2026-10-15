//! Forensic parser for Windows registry hive files (`regf`).
//!
//! * Streams cells from any [`ReadAt`](pm_core::ReadAt) source (no memory
//!   mapping, no whole-file reads); decoded cells sit in a bounded LRU cache,
//!   so hives of hundreds of MB are walked in constant memory.
//! * Keys (`nk`), values (`vk`), every subkey-list flavour (`li`, `lf`,
//!   `lh`, `ri`), big data (`db`), class names and security cells (`sk`).
//! * Case-insensitive path lookups that use the `lf`/`lh` hints before
//!   loading a key.
//! * Transaction-log replay for dirty hives: the old format (`DIRT` dirty
//!   vector, Windows XP–8) and the new format (`HvLE` log entries with
//!   Marvin32 hashes, Windows 8.1+), see [`Hive::open_with_logs`].
//! * Deleted-key recovery from free cells ([`Hive::deleted_keys`]).
//!
//! Hostile input never panics: every read is bounds-checked, cell sizes
//! and list lengths are capped, and recursion (nested `ri` lists, parent
//! chains) is depth-limited.

mod base_block;
#[cfg(feature = "builder")]
pub mod builder;
mod data;
mod deleted;
mod hive;
mod log;
mod value;

pub use base_block::{BaseBlock, HiveFileType};
pub use deleted::{DeletedKey, DeletedValue};
pub use hive::{Hive, Key, Value};
pub use log::ReplayReport;
pub use value::{ValueType, decode_multi_sz, decode_sz};

/// Registry parsing errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a registry hive (missing regf signature)")]
    NotRegf,
    #[error("invalid cell at offset {offset:#x}: {reason}")]
    BadCell { offset: u32, reason: &'static str },
    #[error("truncated structure: {0}")]
    Truncated(#[from] pm_core::Truncated),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

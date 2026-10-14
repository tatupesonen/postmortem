//! Synthetic evidence builders for tests.
//!
//! Builders produce small, fully specified byte images so parser tests do not
//! depend on large binary fixtures. Builders may panic on misuse — they are
//! test-only code.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::missing_panics_doc
)]

pub mod disk;
pub mod tsk;

pub use disk::{DiskBuilder, GptPart, MbrPart};

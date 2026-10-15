//! Synthetic evidence builders for tests.
//!
//! Builders produce small, fully specified byte images so parser tests do not
//! depend on large binary fixtures. Builders may panic on misuse — they are
//! test-only code.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::pedantic,
    reason = "test-only fixture builders: panics on misuse are intended, style lints relaxed"
)]

pub mod demo;
pub mod disk;
pub mod evt;
pub mod ext;
pub mod lznt1;
pub mod ntfs;
pub mod synth;
pub mod tsk;

pub use disk::{DiskBuilder, GptPart, MbrPart};

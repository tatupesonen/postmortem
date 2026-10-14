//! Core primitives shared by every postmortem crate.
//!
//! * [`io`] — the [`ReadAt`](io::ReadAt) random-access byte-source abstraction
//!   that layers images, volumes, filesystems and files.
//! * [`decode`] — bounds-checked binary field decoding for hostile input.
//! * [`time`] — epoch conversions (`FILETIME`, HFS, Cocoa, `WebKit`, DOS, ...).
//! * [`units`] — human-readable (metric by default) size formatting.

pub mod decode;
pub mod io;
pub mod time;
pub mod units;

pub use decode::{Bytes, Truncated};
pub use io::{ReadAt, SharedSource};

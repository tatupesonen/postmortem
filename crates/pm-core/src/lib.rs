//! Core primitives shared by every postmortem crate.
//!
//! * [`io`] — the [`ReadAt`](io::ReadAt) random-access byte-source abstraction
//!   that layers images, volumes, filesystems and files.
//! * [`decode`] — bounds-checked binary field decoding for hostile input.
//! * [`time`] — epoch conversions (FILETIME, HFS, Cocoa, WebKit, DOS, ...).
//! * [`units`] — human-readable (metric by default) size formatting.
//! * [`cache`] — sharded, thread-safe LRU cache for decoded structures.

pub mod cache;
pub mod category;
pub mod decode;
pub mod io;
pub mod time;
pub mod units;
pub mod windows_zones;

pub use decode::{Bytes, Truncated};
pub use io::{ReadAt, SharedSource};

//! Windows shell artefacts.
//!
//! * [`ShellLink`]: the MS-SHLLINK `.lnk` format (header times, target ID
//!   list, link info with local/network paths, string data, extra data
//!   blocks such as the distributed-link-tracker block with the machine
//!   name and MAC address).
//! * [`shell_item`]: shell item ID lists as found in `.lnk` files and the
//!   registry's shellbags (`BagMRU`), turned into readable paths with the
//!   timestamps the items carry.
//! * [`jumplist`]: `*.automaticDestinations-ms` (OLE compound file with one
//!   LNK stream per entry plus the `DestList` MRU stream) and
//!   `*.customDestinations-ms` (concatenated LNKs).
//!
//! All parsers take byte slices of bounded size and never panic on
//! malformed input.

#[cfg(feature = "builder")]
pub mod builder;
pub mod cfb;
pub mod jumplist;
mod link;
pub mod shell_item;

pub use link::{ExtraData, LinkInfo, ShellLink, TrackerData};

/// Parse errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a shell link (bad header)")]
    NotLink,
    #[error("not an OLE compound file")]
    NotCompoundFile,
    #[error("malformed structure: {0}")]
    Malformed(&'static str),
    #[error("truncated structure: {0}")]
    Truncated(#[from] pm_core::Truncated),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// FILETIME → Unix seconds (`None` for 0 or out of range).
pub fn filetime_to_unix(ft: u64) -> Option<i64> {
    pm_core::time::from_filetime(ft).map(jiff::Timestamp::as_second)
}

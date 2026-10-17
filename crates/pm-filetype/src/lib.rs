//! File type identification: a pure-Rust port of the detection Autopsy
//! performs with Apache Tika 3.0.0 (`tika-mimetypes.xml` magic, globs and
//! type hierarchy, XML root elements, text statistics and Tika's container
//! detectors for OLE2, ZIP packages, Ogg, binary plists and gzip), plus
//! Autopsy's own `FileTypeDetector` rules and user-defined types.
//!
//! * [`FileTypeDetector::detect`]: Autopsy's MIME type for a file.
//! * [`tika_detect`]: what `new Tika().detect(stream[, name])` answers.
//! * [`cfb`]: an OLE2 compound-file reader (POI-compatible validation).
//! * [`has_encrypted_entry`]: ZIP encryption flags.
//!
//! The Tika database is embedded verbatim (Apache-2.0, see
//! `data/TIKA-NOTICE.txt`) and parsed on first use.

mod archive;
mod autopsy;
mod bplist;
pub mod cfb;
mod db;
mod magic;
pub mod media;
mod ogg;
mod ole;
mod stream;
pub mod text;
mod tika;
mod xml;
mod zip;

pub use autopsy::{
    FileClass, FileType, FileTypeDetector, Signature, SignatureKind, TypeError,
    autopsy_defined_types, detected_types, import_legacy_xml, strip_params, tika_detectable_types,
};
pub use db::{DbError, MAGIC_PREFIX_LEN, MimeDb};
pub use magic::MagicError;
pub use tika::detect as tika_detect;
pub use zip::has_encrypted_entry;
#[cfg(any(test, feature = "builder"))]
pub use zip::{ZipBuilder, build_zip};

/// A detector failed the way a Tika detector throws; Autopsy then records
/// `application/octet-stream`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure(pub(crate) String);

/// Verbatim Tika notice for the embedded database.
pub const TIKA_NOTICE: &str = include_str!("../data/TIKA-NOTICE.txt");

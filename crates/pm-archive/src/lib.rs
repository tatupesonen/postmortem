//! Streaming readers for archives and embedded content.
//!
//! Every container format sits behind one API: [`extract`] walks the
//! members of a container read from a [`ReadAt`] and hands each one to a
//! caller-supplied [`Sink`] (metadata first, then the decoded bytes in
//! chunks). Nothing is ever loaded whole into memory except where a format
//! demands it, and then only below [`Options::max_in_memory`].
//!
//! Formats: zip (stored, deflate, bzip2, LZMA, zstd, xz; `ZipCrypto` and AES
//! entries reported as encrypted), 7z (AES blocks reported encrypted), tar
//! (ustar, GNU long names, PAX), gzip / bzip2 / xz / zstd single streams,
//! RAR 1.3 to 7 (`rars`), ISO 9660 with Joliet and Rock Ridge names,
//! Office Open XML media and embeddings, the pictures of legacy Word,
//! Excel and `PowerPoint` files (BLIP records in compound-file streams) and
//! PDF attachments and images.
//!
//! **Zip bombs.** Besides Autopsy's rule (an item whose *declared*
//! uncompressed size is at least 500 MB with a compression ratio of 600 or
//! more), the same ratio is applied to the bytes *actually decoded*, per
//! member and per container, while streaming. This catches archives with
//! lying headers, single-stream formats and overlapping-entry
//! ("non-recursive") bombs. Member counts, decoder dictionaries and
//! in-memory buffers are capped as well ([`Options`]). Nesting depth is the
//! caller's concern (the Embedded File Extractor tracks it per job).
//!
//! **File names.** Entries carry the raw name bytes where the format stores
//! bytes; [`names::repair`] re-decodes a directory's names when they are
//! not UTF-8 (Autopsy's charset fix for CP437 / Shift-JIS zip names).

#![allow(
    clippy::many_single_char_names,
    clippy::single_match_else,
    reason = "binary-format code uses short field names"
)]

mod cfb;
mod detect;
mod driver;
mod io;
mod iso;
pub mod names;
mod office;
mod pdf;
mod rar;
mod sevenz;
mod single;
mod tar;
mod zip;

use std::path::PathBuf;

pub use detect::{Format, detect, detect_head, format_for_mime};
pub use driver::{BombReason, Summary};
use pm_core::ReadAt;

/// Limits and settings of one extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Compression ratio (decoded / packed) at or above which an item is a
    /// possible zip bomb (Autopsy `MAX_COMPRESSION_RATIO`).
    pub max_ratio: u64,
    /// Smallest decoded size the ratio is checked for (Autopsy
    /// `MIN_COMPRESSION_RATIO_SIZE`, 500 MB).
    pub ratio_min_size: u64,
    /// Apply the ratio to the sizes the archive *declares* before decoding
    /// (Autopsy's only check). Autopsy skips it for deleted archives,
    /// whose headers are often damaged; the decoded-bytes meter always
    /// runs.
    pub check_declared: bool,
    /// Most members read from one container.
    pub max_members: u64,
    /// Largest decoder dictionary / window accepted (LZMA, xz, zstd, RAR).
    pub max_dictionary: u64,
    /// Largest buffer held in memory for formats that need random access
    /// to a whole stream (RAR parsing, PDF object streams, Excel drawing
    /// groups); bigger RAR inputs are spooled to a temporary file.
    pub max_in_memory: u64,
    /// Where temporary files go (`None`: the system temporary folder).
    pub temp_dir: Option<PathBuf>,
    /// Password for encrypted members (`None`: encrypted members are
    /// reported, not decoded).
    pub password: Option<Vec<u8>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_ratio: 600,
            ratio_min_size: 500_000_000,
            check_declared: true,
            max_members: 500_000,
            max_dictionary: 256 * 1024 * 1024,
            max_in_memory: 64 * 1024 * 1024,
            temp_dir: None,
            password: None,
        }
    }
}

/// One member of a container, announced before its data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entry {
    /// Position in the container (0-based, in archive order).
    pub index: u64,
    /// Path inside the container as decoded by the format (`/` or `\`
    /// separated). Empty when the format stores no name (single-stream
    /// compressors without a stored name).
    pub path: String,
    /// The undecoded path bytes, where the format stores bytes in an
    /// unspecified code page (zip without the UTF-8 flag, tar, RAR 1.x–4).
    pub raw_path: Option<Vec<u8>>,
    pub is_dir: bool,
    /// Uncompressed size declared by the container.
    pub size: Option<u64>,
    /// Compressed size declared by the container.
    pub packed_size: Option<u64>,
    /// The member is encrypted and no (correct) password was supplied, so
    /// its data is not delivered.
    pub encrypted: bool,
    /// Unix seconds.
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
}

/// How a member ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberEnd {
    /// All data delivered (CRC verified where the format has one).
    Complete,
    /// Data delivered, but the stored checksum does not match.
    ChecksumMismatch,
    /// Decoding failed part-way; the data delivered so far is kept.
    Failed(String),
    /// Not decoded (encrypted, unsupported method, …).
    Skipped(String),
}

/// Receives the members of a container.
pub trait Sink {
    /// A new member. Return `false` to skip its data (`end` is still
    /// called). Directories and encrypted members never receive data.
    fn begin(&mut self, entry: &Entry) -> std::io::Result<bool>;
    /// Decoded data of the current member.
    fn write(&mut self, data: &[u8]) -> std::io::Result<()>;
    /// End of the current member.
    fn end(&mut self, end: &MemberEnd) -> std::io::Result<()>;
    /// Polled while decoding; `true` stops the extraction.
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Why an extraction stopped.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a valid {format} container: {reason}")]
    Malformed {
        format: &'static str,
        reason: String,
    },
    #[error("unsupported {0}")]
    Unsupported(String),
    /// The container's directory is encrypted: nothing can be listed.
    #[error("the archive is password protected")]
    Encrypted,
    #[error("possible zip bomb: {0}")]
    Bomb(BombReason),
    #[error("limit exceeded: {0}")]
    Limit(String),
    #[error("cancelled")]
    Cancelled,
    /// The sink failed (e.g. the case volume is full).
    #[error("output error: {0}")]
    Sink(std::io::Error),
}

impl ArchiveError {
    pub(crate) fn malformed(format: &'static str, reason: impl Into<String>) -> Self {
        Self::Malformed {
            format,
            reason: reason.into(),
        }
    }
}

/// Extracts every member of a container of the given format.
///
/// Members that fail to decode are reported through [`Sink::end`] and the
/// walk continues where the format allows it. The returned [`Summary`]
/// counts members, encrypted members and decoded bytes. Zip-bomb and limit
/// violations, cancellation and sink errors stop the walk with an error.
pub fn extract(
    format: Format,
    src: &dyn ReadAt,
    opts: &Options,
    sink: &mut dyn Sink,
) -> Result<Summary, ArchiveError> {
    let mut d = driver::Driver::new(src.size(), opts, sink);
    match format {
        Format::Zip => zip::extract(src, &mut d, None)?,
        Format::SevenZip => sevenz::extract(src, &mut d)?,
        Format::Rar => rar::extract(src, &mut d)?,
        Format::Tar => tar::extract(src, &mut d)?,
        Format::Gzip | Format::Bzip2 | Format::Xz | Format::Zstd => {
            single::extract(format, src, &mut d)?;
        }
        Format::Iso9660 => iso::extract(src, &mut d)?,
        Format::Docx | Format::Xlsx | Format::Pptx => office::extract_ooxml(src, &mut d)?,
        Format::Doc | Format::Xls | Format::Ppt => office::extract_ole(format, src, &mut d)?,
        Format::Pdf => pdf::extract(src, &mut d)?,
    }
    Ok(d.finish())
}

/// Autopsy's name for a member whose container stores none
/// (`SevenZipExtractor.getPathInArchive`): `x.gz` → `x`, `x.tgz` →
/// `x.tar`, `x.bz2` → `x`, otherwise `/<archive name>/<index>`. postmortem
/// adds `.xz`/`.txz` and `.zst`/`.tzst`. An alternate-data-stream suffix
/// (`:stream`) is dropped from the extension.
pub fn default_member_name(archive_name: &str, index: u64) -> String {
    if let Some(dot) = archive_name.rfind('.') {
        let base = archive_name.get(..dot).unwrap_or_default();
        let mut ext = archive_name.get(dot..).unwrap_or_default();
        if let Some(colon) = ext.rfind(':') {
            ext = ext.get(..colon).unwrap_or_default();
        }
        let name = match ext.to_ascii_lowercase().as_str() {
            ".gz" | ".bz2" | ".xz" | ".zst" => Some(base.to_owned()),
            ".tgz" | ".txz" | ".tzst" | ".tbz2" => Some(format!("{base}.tar")),
            _ => None,
        };
        if let Some(n) = name.filter(|n| !n.is_empty()) {
            return format!("/{n}");
        }
    }
    format!("/{archive_name}/{index}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_names_follow_autopsy() {
        assert_eq!(default_member_name("a.gz", 0), "/a");
        assert_eq!(default_member_name("a.tgz", 0), "/a.tar");
        assert_eq!(default_member_name("a.bz2", 0), "/a");
        assert_eq!(default_member_name("a.tar.xz", 0), "/a.tar");
        assert_eq!(default_member_name("a.gz:ads", 0), "/a");
        assert_eq!(default_member_name("blob", 3), "/blob/3");
        assert_eq!(default_member_name(".gz", 0), "/.gz/0");
        assert_eq!(default_member_name("x.zip", 1), "/x.zip/1");
    }
}

//! Streaming text extraction for keyword search (Autopsy's
//! `textextractors` package and the parts of Apache Tika that Autopsy
//! relies on, in pure Rust).
//!
//! * [`kind_for_mime`] picks an extractor in Autopsy's
//!   `TextExtractorFactory` order (plain text → HTML ≤ 50 MB → SQLite →
//!   the document formats Autopsy hands to Tika); [`sniff_mime`] gives a
//!   coarse MIME type when File Type Identification has not run.
//! * [`extract`] streams text into a [`TextSink`] (which can stop the
//!   extraction: cancellation, size limits) and returns the document's
//!   metadata under Tika's names (`Author`, `Creation-Date`, …).
//! * [`strings`] ports Autopsy's two string scanners (English-only and
//!   international with its per-code-point script table).
//! * [`Chunker`] ports Autopsy's `Chunker` (≈ 32 KB chunks with an
//!   overlap window, NFKC, Solr-invalid characters replaced).
//! * [`metadata_block`] and [`artifact_text`] build the extra text Autopsy
//!   indexes.
//!
//! Every extractor treats its input as hostile: sizes are capped, loops are
//! bounded and panics inside third-party parsers are caught by
//! [`extract`].

mod chunker;
mod email;
pub mod encoding;
mod html;
mod mime;
mod office;
mod ole;
mod pdf;
mod plain;
mod rtf;
mod sink;
mod sqlite;
pub mod strings;
mod util;
mod xml;

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use pm_core::{ReadAt, SharedSource};
use thiserror::Error;

pub use chunker::{Chunk, Chunker, ChunkingSink, is_valid_solr_char, java_is_whitespace, sanitize};
pub use mime::{
    ARCHIVE_MIME_TYPES, HTML_MAX_SIZE, Kind, OCTET_STREAM, WEB_MIME_TYPES, kind_for_mime,
    sniff_mime,
};
pub use sink::{LimitSink, Stop, StringSink, TextSink};
pub use util::java_double_to_string;

/// Document metadata: Tika-style key → value, sorted by key.
pub type Metadata = BTreeMap<String, String>;

/// Why an extraction failed.
#[derive(Debug, Error)]
pub enum TextError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the document is encrypted")]
    Encrypted,
    #[error("malformed {format}: {msg}")]
    Malformed { format: &'static str, msg: String },
    #[error("the document is too large ({size} bytes, limit {limit})")]
    TooLarge { size: u64, limit: u64 },
    #[error("extraction was stopped")]
    Stopped,
    #[error("the {0} extractor panicked")]
    Panicked(&'static str),
}

impl TextError {
    pub(crate) fn malformed(format: &'static str, msg: impl std::fmt::Display) -> Self {
        Self::Malformed {
            format,
            msg: msg.to_string(),
        }
    }
}

impl From<Stop> for TextError {
    fn from(_: Stop) -> Self {
        Self::Stopped
    }
}

/// Settings that some extractors need.
#[derive(Debug, Clone, Default)]
pub struct ExtractOptions {
    /// Folder for temporary copies (SQLite databases are copied out like
    /// Autopsy does); the system temporary folder when `None`.
    pub temp_dir: Option<PathBuf>,
    /// The database's `-wal` file, copied next to it so committed but
    /// not yet checkpointed rows are seen (Autopsy copies `-wal`/`-shm`).
    pub sqlite_wal: Option<SharedSource>,
}

/// Extracts the text of `src` as `kind`, pushing it into `sink`, and
/// returns the document metadata.
///
/// Panics in the parsers are caught and reported as
/// [`TextError::Panicked`]; text pushed before the failure stays pushed.
pub fn extract(
    kind: Kind,
    src: &dyn ReadAt,
    sink: &mut dyn TextSink,
    opts: &ExtractOptions,
) -> Result<Metadata, TextError> {
    extract_at_depth(kind, src, sink, opts, 0)
}

/// Embedded documents (e-mail attachments) are extracted this many
/// levels deep.
pub(crate) const MAX_NESTING: u32 = 2;

fn extract_at_depth(
    kind: Kind,
    src: &dyn ReadAt,
    sink: &mut dyn TextSink,
    opts: &ExtractOptions,
    depth: u32,
) -> Result<Metadata, TextError> {
    let run = AssertUnwindSafe(|| match kind {
        Kind::Plain => plain::extract(src, sink),
        Kind::Html => html::extract(src, sink),
        Kind::Xml => xml::extract(src, sink),
        Kind::Sqlite => sqlite::extract(src, sink, opts),
        Kind::Pdf => pdf::extract(src, sink),
        Kind::Ooxml => office::extract_ooxml(src, sink),
        Kind::Odf => office::extract_odf(src, sink),
        Kind::Epub => office::extract_epub(src, sink),
        Kind::Ole => ole::extract_depth(src, sink, depth),
        Kind::Rtf => rtf::extract(src, sink),
        Kind::Email => email::extract(src, sink, depth),
    });
    if let Ok(r) = catch_unwind(run) {
        r
    } else {
        tracing::warn!(?kind, "text extractor panicked");
        Err(TextError::Panicked(kind.name()))
    }
}

/// Extracts an embedded document (attachment) found at `depth`; failures
/// other than a stop are ignored. SQLite attachments are skipped (they
/// would need temporary files).
pub(crate) fn extract_nested(
    data: Vec<u8>,
    name: &str,
    sink: &mut dyn TextSink,
    depth: u32,
) -> Result<(), Stop> {
    if depth + 1 > MAX_NESTING {
        return Ok(());
    }
    let head = data.get(..data.len().min(8192)).unwrap_or_default();
    let mime = sniff_mime(head, name);
    let Some(kind) = kind_for_mime(mime, data.len() as u64) else {
        return Ok(());
    };
    if kind == Kind::Sqlite {
        return Ok(());
    }
    let src = pm_core::io::MemSource::new(data);
    nested_result(&extract_at_depth(
        kind,
        &src,
        sink,
        &ExtractOptions::default(),
        depth + 1,
    ))
}

/// A nested extraction's outcome: only a stop matters to the caller.
pub(crate) fn nested_result(r: &Result<Metadata, TextError>) -> Result<(), Stop> {
    match r {
        Err(TextError::Stopped) => Err(Stop),
        _ => Ok(()),
    }
}

/// Autopsy's metadata separator line, appended to every extracted text
/// (`KeywordSearchIngestModule.getMetaDataCharSource`): two newlines, the
/// title line, two newlines, then `key: value` lines sorted by key.
pub fn metadata_block(metadata: &Metadata) -> String {
    let mut s = String::from(
        "\n\n------------------------------METADATA------------------------------\n\n",
    );
    let mut first = true;
    for (k, v) in metadata {
        if !first {
            s.push('\n');
        }
        first = false;
        s.push_str(k);
        s.push_str(": ");
        s.push_str(v);
    }
    s
}

/// Autopsy's `ArtifactTextExtractor`: one `<attribute display name> :
/// <value>` line per attribute. Date-time values must already be
/// formatted by the caller.
pub fn artifact_text<'a>(attributes: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut s = String::new();
    for (name, value) in attributes {
        s.push_str(name);
        s.push_str(" : ");
        s.push_str(value);
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_block_format() {
        let mut m = Metadata::new();
        m.insert("Title".into(), "Report".into());
        m.insert("Author".into(), "Jane".into());
        assert_eq!(
            metadata_block(&m),
            "\n\n------------------------------METADATA------------------------------\n\nAuthor: Jane\nTitle: Report"
        );
        assert_eq!(
            metadata_block(&Metadata::new()),
            "\n\n------------------------------METADATA------------------------------\n\n"
        );
    }

    #[test]
    fn artifact_text_lines() {
        assert_eq!(
            artifact_text([("URL", "http://a"), ("Title", "x")]),
            "URL : http://a\nTitle : x\n"
        );
    }

    #[test]
    fn panics_are_caught() {
        // A PDF whose page tree is broken in a way pdf-extract unwraps on is
        // covered in pdf.rs; here: extraction of garbage never panics.
        let src = pm_core::io::MemSource::new(b"%PDF-1.4 garbage".to_vec());
        let mut sink = StringSink::default();
        let r = extract(Kind::Pdf, &src, &mut sink, &ExtractOptions::default());
        assert!(r.is_err());
    }
}

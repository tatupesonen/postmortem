//! Signature detection and MIME mapping.

use pm_core::ReadAt;

use crate::io::read_upto;

/// Container formats [`crate::extract`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Zip,
    SevenZip,
    Rar,
    Tar,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    Iso9660,
    /// Office Open XML (media and embeddings are extracted).
    Docx,
    Xlsx,
    Pptx,
    /// Legacy binary Office (pictures are extracted).
    Doc,
    Xls,
    Ppt,
    /// Attachments and images.
    Pdf,
}

impl Format {
    /// The MIME type Apache Tika (Autopsy's File Type Identification)
    /// reports for the format.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Zip => "application/zip",
            Self::SevenZip => "application/x-7z-compressed",
            Self::Rar => "application/x-rar-compressed",
            Self::Tar => "application/x-tar",
            Self::Gzip => "application/gzip",
            Self::Bzip2 => "application/x-bzip2",
            Self::Xz => "application/x-xz",
            Self::Zstd => "application/zstd",
            Self::Iso9660 => "application/x-iso9660-image",
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
            Self::Doc => "application/msword",
            Self::Xls => "application/vnd.ms-excel",
            Self::Ppt => "application/vnd.ms-powerpoint",
            Self::Pdf => "application/pdf",
        }
    }

    /// Archives keep their directory tree; documents yield a flat list of
    /// embedded items.
    pub fn is_archive(self) -> bool {
        !self.is_document()
    }

    /// Office or PDF document (Autopsy's `DocumentEmbeddedContentExtractor`).
    pub fn is_document(self) -> bool {
        matches!(
            self,
            Self::Docx | Self::Xlsx | Self::Pptx | Self::Doc | Self::Xls | Self::Ppt | Self::Pdf
        )
    }
}

/// The format for a MIME type (as set by File Type Identification).
/// Accepts Tika's aliases (`application/x-gzip`, `application/x-gtar`,
/// `application/vnd.rar`, …); parameters (`; charset=…`) are ignored.
pub fn format_for_mime(mime: &str) -> Option<Format> {
    let m = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    Some(match m.as_str() {
        "application/zip" | "application/x-zip-compressed" => Format::Zip,
        "application/x-7z-compressed" => Format::SevenZip,
        "application/x-rar-compressed" | "application/vnd.rar" | "application/x-rar" => Format::Rar,
        "application/x-tar" | "application/x-gtar" | "application/x-ustar" => Format::Tar,
        "application/gzip" | "application/x-gzip" => Format::Gzip,
        "application/x-bzip2" | "application/x-bzip" => Format::Bzip2,
        "application/x-xz" => Format::Xz,
        "application/zstd" | "application/x-zstd" => Format::Zstd,
        "application/x-iso9660-image" => Format::Iso9660,
        "application/pdf" => Format::Pdf,
        "application/msword" => Format::Doc,
        "application/vnd.ms-excel" => Format::Xls,
        "application/vnd.ms-powerpoint" => Format::Ppt,
        _ => {
            return [Format::Docx, Format::Xlsx, Format::Pptx]
                .into_iter()
                .find(|f| f.mime() == m);
        }
    })
}

const OLE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Detects a format from the first bytes alone (no zip/OLE refinement:
/// zip-based Office files report [`Format::Zip`], compound files report
/// `None`). `head` should hold at least 512 bytes when available.
pub fn detect_head(head: &[u8]) -> Option<Format> {
    let starts = |sig: &[u8]| head.starts_with(sig);
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") || starts(b"PK\x07\x08") {
        return Some(Format::Zip);
    }
    if starts(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(Format::SevenZip);
    }
    if starts(b"Rar!\x1a\x07\x00") || starts(b"Rar!\x1a\x07\x01\x00") || starts(b"RE~^") {
        return Some(Format::Rar);
    }
    if starts(&[0x1F, 0x8B, 0x08]) {
        return Some(Format::Gzip);
    }
    if head.len() >= 10
        && starts(b"BZh")
        && head.get(3).is_some_and(|b| (b'1'..=b'9').contains(b))
        && head.get(4..10) == Some(&[0x31, 0x41, 0x59, 0x26, 0x53, 0x59][..])
    {
        return Some(Format::Bzip2);
    }
    if starts(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some(Format::Xz);
    }
    if starts(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some(Format::Zstd);
    }
    if is_pdf(head) {
        return Some(Format::Pdf);
    }
    if crate::tar::looks_like_tar(head) {
        return Some(Format::Tar);
    }
    None
}

/// `%PDF-` within the first 1024 bytes (readers tolerate leading junk).
fn is_pdf(head: &[u8]) -> bool {
    let h = head.get(..head.len().min(1024)).unwrap_or_default();
    memchr::memmem::find(h, b"%PDF-").is_some()
}

/// Detects a container format by signature, refining zip files into
/// Office Open XML documents and compound files into Word, Excel or
/// `PowerPoint`, and finding ISO 9660 images (signature at 32 KiB).
pub fn detect(src: &dyn ReadAt) -> Option<Format> {
    let head = read_upto(src, 0, 4096).ok()?;
    if head.starts_with(&OLE) {
        return crate::office::ole_kind(src);
    }
    match detect_head(&head) {
        Some(Format::Zip) => Some(crate::office::ooxml_kind(src).unwrap_or(Format::Zip)),
        Some(f) => Some(f),
        None => crate::iso::is_iso(src).then_some(Format::Iso9660),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_round_trip() {
        for f in [
            Format::Zip,
            Format::SevenZip,
            Format::Rar,
            Format::Tar,
            Format::Gzip,
            Format::Bzip2,
            Format::Xz,
            Format::Zstd,
            Format::Iso9660,
            Format::Docx,
            Format::Xlsx,
            Format::Pptx,
            Format::Doc,
            Format::Xls,
            Format::Ppt,
            Format::Pdf,
        ] {
            assert_eq!(format_for_mime(f.mime()), Some(f), "{f:?}");
        }
        assert_eq!(format_for_mime("application/x-gzip"), Some(Format::Gzip));
        assert_eq!(format_for_mime("Application/X-GTAR"), Some(Format::Tar));
        assert_eq!(format_for_mime("application/vnd.rar"), Some(Format::Rar));
        assert_eq!(
            format_for_mime("application/pdf; version=1.7"),
            Some(Format::Pdf)
        );
        assert_eq!(format_for_mime("application/java-archive"), None);
        assert_eq!(format_for_mime("text/plain"), None);
        assert!(Format::Pdf.is_document() && Format::Zip.is_archive());
    }

    #[test]
    fn signatures() {
        assert_eq!(detect_head(b"PK\x03\x04rest"), Some(Format::Zip));
        assert_eq!(
            detect_head(b"7z\xBC\xAF\x27\x1C\x00\x04"),
            Some(Format::SevenZip)
        );
        assert_eq!(detect_head(b"Rar!\x1a\x07\x01\x00"), Some(Format::Rar));
        assert_eq!(detect_head(b"\x1f\x8b\x08\x00"), Some(Format::Gzip));
        assert_eq!(detect_head(b"BZh91AY&SY...."), Some(Format::Bzip2));
        assert_eq!(detect_head(b"BZh01AY&SY...."), None);
        assert_eq!(detect_head(b"\xFD7zXZ\x00"), Some(Format::Xz));
        assert_eq!(detect_head(b"\x28\xB5\x2F\xFD"), Some(Format::Zstd));
        assert_eq!(detect_head(b"junk%PDF-1.4"), Some(Format::Pdf));
        assert_eq!(detect_head(b"hello"), None);
        assert_eq!(detect_head(b""), None);
    }
}

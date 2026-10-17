//! Choosing an extractor from a MIME type (Autopsy
//! `TextExtractorFactory`), and a coarse MIME sniffer for files that File
//! Type Identification has not typed.

/// Autopsy `KeywordSearchIngestModule.ARCHIVE_MIME_TYPES`: only the file's
/// metadata is indexed (archives are handled by the embedded-file
/// extractor). Note `application/x-executable` is in the list, so ELF
/// binaries get no string extraction (Autopsy behaviour, kept).
pub const ARCHIVE_MIME_TYPES: &[&str] = &[
    "application/x-7z-compressed",
    "application/x-ace-compressed",
    "application/x-alz-compressed",
    "application/x-arj",
    "application/vnd.ms-cab-compressed",
    "application/x-cfs-compressed",
    "application/x-dgc-compressed",
    "application/x-apple-diskimage",
    "application/x-gca-compressed",
    "application/x-dar",
    "application/x-lzx",
    "application/x-lzh",
    "application/x-rar-compressed",
    "application/x-stuffit",
    "application/x-stuffitx",
    "application/x-gtar",
    "application/x-archive",
    "application/x-executable",
    "application/x-gzip",
    "application/zip",
    "application/x-zoo",
    "application/x-cpio",
    "application/x-shar",
    "application/x-tar",
    "application/x-bzip",
    "application/x-bzip2",
    "application/x-lzip",
    "application/x-lzma",
    "application/x-lzop",
    "application/x-z",
    "application/x-compress",
];

/// MIME type of unstructured binary data: string extraction.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// Autopsy `HtmlTextExtractor.WEB_MIME_TYPES`.
pub const WEB_MIME_TYPES: &[&str] = &[
    "application/javascript",
    "application/xhtml+xml",
    "application/json",
    "text/css",
    "text/html",
    "text/javascript",
];

/// Autopsy's HTML extractor size limit (bytes).
pub const HTML_MAX_SIZE: u64 = 50_000_000;

/// Which extractor handles a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `text/*`, decoded with encoding detection.
    Plain,
    /// HTML, XHTML and the other web types (Jericho-style rendering).
    Html,
    /// Generic XML (text nodes).
    Xml,
    /// SQLite database (Autopsy's table dump).
    Sqlite,
    Pdf,
    /// Office Open XML (DOCX, XLSX, PPTX and their variants).
    Ooxml,
    /// `OpenDocument` (ODT, ODS, ODP, ODG).
    Odf,
    Epub,
    /// OLE2 compound documents: Word 97+, Excel BIFF8, `PowerPoint` 97+,
    /// Outlook `.msg`, and summary properties of anything else.
    Ole,
    Rtf,
    /// RFC 822 / MIME message.
    Email,
}

impl Kind {
    /// Short name for logs and errors.
    pub fn name(self) -> &'static str {
        match self {
            Self::Plain => "text",
            Self::Html => "HTML",
            Self::Xml => "XML",
            Self::Sqlite => "SQLite",
            Self::Pdf => "PDF",
            Self::Ooxml => "OOXML",
            Self::Odf => "ODF",
            Self::Epub => "EPUB",
            Self::Ole => "OLE",
            Self::Rtf => "RTF",
            Self::Email => "e-mail",
        }
    }

    /// Formats Autopsy hands to Tika: empty output counts as a failure and
    /// the file falls back to string extraction.
    pub fn is_document(self) -> bool {
        !matches!(self, Self::Plain | Self::Html | Self::Sqlite)
    }
}

/// The extractor for a file of type `mime` and `size` bytes, in Autopsy's
/// `TextExtractorFactory` order: `text/plain` → web types ≤ 50 MB →
/// SQLite → the Tika-handled formats postmortem supports. `None` means
/// "no extractor" (the caller falls back to string extraction).
pub fn kind_for_mime(mime: &str, size: u64) -> Option<Kind> {
    let mime = mime.trim();
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    let lower = mime.to_ascii_lowercase();
    let m = lower.as_str();
    if m == "text/plain" {
        return Some(Kind::Plain);
    }
    if WEB_MIME_TYPES.contains(&m) && size <= HTML_MAX_SIZE {
        return Some(Kind::Html);
    }
    if m == "application/x-sqlite3" {
        return Some(Kind::Sqlite);
    }
    // Tika: never for binary blobs, archives, video (except FLV) or SQLite.
    if m == OCTET_STREAM
        || m == "application/x-msdownload"
        || ARCHIVE_MIME_TYPES.contains(&m)
        || (m.starts_with("video/") && m != "video/x-flv")
    {
        return None;
    }
    Some(match m {
        "application/pdf" => Kind::Pdf,
        "application/rtf" | "text/rtf" => Kind::Rtf,
        "message/rfc822" => Kind::Email,
        "application/epub+zip" => Kind::Epub,
        "application/msword"
        | "application/vnd.ms-excel"
        | "application/vnd.ms-powerpoint"
        | "application/vnd.ms-outlook"
        | "application/x-tika-msoffice"
        | "application/x-tika-msworks-spreadsheet"
        | "application/vnd.ms-word.document"
        | "application/x-mspublisher" => Kind::Ole,
        "application/x-tika-ooxml" => Kind::Ooxml,
        // Large HTML goes to Tika's HTML parser.
        "text/html" | "application/xhtml+xml" => Kind::Html,
        "application/xml" | "text/xml" => Kind::Xml,
        _ if m.starts_with("application/vnd.openxmlformats-officedocument.")
            || m.starts_with("application/vnd.ms-word.")
            || m.starts_with("application/vnd.ms-excel.")
            || m.starts_with("application/vnd.ms-powerpoint.") =>
        {
            Kind::Ooxml
        }
        _ if m.starts_with("application/vnd.oasis.opendocument.") => Kind::Odf,
        _ if m.ends_with("+xml") => Kind::Xml,
        _ if m.starts_with("text/") && m != "text/css" && m != "text/javascript" => Kind::Plain,
        _ => return None,
    })
}

/// A coarse MIME type from a file's first bytes and its name, for files
/// File Type Identification has not typed. Returns
/// [`OCTET_STREAM`] for unrecognised binary content.
pub fn sniff_mime(head: &[u8], name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let starts = |p: &[u8]| head.starts_with(p);
    if head.is_empty() {
        return OCTET_STREAM;
    }
    if starts(b"%PDF-") {
        return "application/pdf";
    }
    if starts(b"SQLite format 3\0") {
        return "application/x-sqlite3";
    }
    if starts(b"{\\rtf") {
        return "application/rtf";
    }
    if starts(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return match ext.as_str() {
            "doc" | "dot" => "application/msword",
            "xls" | "xlt" | "xla" => "application/vnd.ms-excel",
            "ppt" | "pps" | "pot" => "application/vnd.ms-powerpoint",
            "msg" => "application/vnd.ms-outlook",
            _ => "application/x-tika-msoffice",
        };
    }
    if starts(b"PK\x03\x04") {
        return sniff_zip(head, &ext);
    }
    if starts(&[0x1F, 0x8B]) {
        return "application/gzip";
    }
    if starts(b"7z\xBC\xAF\x27\x1C") {
        return "application/x-7z-compressed";
    }
    if starts(b"Rar!\x1A\x07") {
        return "application/x-rar-compressed";
    }
    if starts(b"\x7FELF") {
        return "application/x-executable";
    }
    if starts(b"MZ") {
        return "application/x-msdownload";
    }
    if starts(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    if starts(b"\x89PNG\r\n\x1A\n") {
        return "image/png";
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return "image/gif";
    }
    sniff_text(head, &ext)
}

fn sniff_zip(head: &[u8], ext: &str) -> &'static str {
    match ext {
        "docx" => return "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docm" => return "application/vnd.ms-word.document.macroenabled.12",
        "xlsx" => return "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsm" => return "application/vnd.ms-excel.sheet.macroenabled.12",
        "pptx" => {
            return "application/vnd.openxmlformats-officedocument.presentationml.presentation";
        }
        "pptm" => return "application/vnd.ms-powerpoint.presentation.macroenabled.12",
        "odt" => return "application/vnd.oasis.opendocument.text",
        "ods" => return "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => return "application/vnd.oasis.opendocument.presentation",
        "epub" => return "application/epub+zip",
        _ => {}
    }
    // First local header: name at 30, stored data after name + extra.
    let name_len = usize::from(crate::util::le16(head, 26).unwrap_or(0));
    let extra_len = usize::from(crate::util::le16(head, 28).unwrap_or(0));
    let name = head.get(30..30 + name_len).unwrap_or_default();
    if name == b"[Content_Types].xml" || name.starts_with(b"_rels/") || name.starts_with(b"word/") {
        return "application/x-tika-ooxml";
    }
    if name == b"mimetype" {
        let data = head.get(30 + name_len + extra_len..).unwrap_or_default();
        for m in [
            "application/vnd.oasis.opendocument.text",
            "application/vnd.oasis.opendocument.spreadsheet",
            "application/vnd.oasis.opendocument.presentation",
            "application/vnd.oasis.opendocument.graphics",
            "application/epub+zip",
        ] {
            if data.starts_with(m.as_bytes()) {
                return m;
            }
        }
    }
    "application/zip"
}

fn sniff_text(head: &[u8], ext: &str) -> &'static str {
    let (body, utf16) = if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) {
        (head, true)
    } else {
        (head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head), false)
    };
    if utf16 {
        return "text/plain";
    }
    let sample = body.get(..body.len().min(8192)).unwrap_or(body);
    let control = sample
        .iter()
        .filter(|&&b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B)) || b == 0x7F)
        .count();
    if control > 0 {
        return OCTET_STREAM;
    }
    // Mostly-binary high bytes that are not UTF-8 (Tika's TextStatistics
    // is more elaborate; this keeps Latin-1 text but rejects noise).
    let high = sample.iter().filter(|&&b| b >= 0x80).count();
    if high * 4 > sample.len() && std::str::from_utf8(sample).is_err() {
        return OCTET_STREAM;
    }
    let trimmed = trim_ascii_start(sample);
    let lower: Vec<u8> = trimmed
        .iter()
        .take(256)
        .map(u8::to_ascii_lowercase)
        .collect();
    if lower.starts_with(b"<!doctype html")
        || lower.starts_with(b"<html")
        || lower.starts_with(b"<head")
        || lower.starts_with(b"<body")
        || lower.starts_with(b"<!--") && ext.starts_with("htm")
        || matches!(ext, "htm" | "html" | "xhtml") && lower.starts_with(b"<")
    {
        return "text/html";
    }
    if lower.starts_with(b"<?xml") {
        if contains(&lower, b"<html") || contains(&lower, b"xhtml") {
            return "application/xhtml+xml";
        }
        return "application/xml";
    }
    if is_mail_header(trimmed) {
        return "message/rfc822";
    }
    match ext {
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "eml" => "message/rfc822",
        _ => "text/plain",
    }
}

fn trim_ascii_start(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|c| c.is_ascii_whitespace()).count();
    b.get(n..).unwrap_or_default()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// RFC 822 headers at the start: the first line is `Name: value` and at
/// least two well-known header names appear at line starts.
fn is_mail_header(b: &[u8]) -> bool {
    const NAMES: &[&[u8]] = &[
        b"from:",
        b"to:",
        b"subject:",
        b"date:",
        b"received:",
        b"message-id:",
        b"return-path:",
        b"mime-version:",
        b"delivered-to:",
        b"x-mailer:",
    ];
    let first = b.split(|&c| c == b'\n').next().unwrap_or_default();
    let Some(colon) = first.iter().position(|&c| c == b':') else {
        return false;
    };
    if colon == 0
        || !first
            .get(..colon)
            .unwrap_or_default()
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'-')
    {
        return false;
    }
    let mut seen = 0;
    for line in b.split(|&c| c == b'\n').take(64) {
        let l: Vec<u8> = line.iter().take(20).map(u8::to_ascii_lowercase).collect();
        if NAMES.iter().any(|n| l.starts_with(n)) {
            seen += 1;
        }
    }
    seen >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_order() {
        assert_eq!(kind_for_mime("text/plain", 10), Some(Kind::Plain));
        assert_eq!(
            kind_for_mime("Text/HTML; charset=utf-8", 10),
            Some(Kind::Html)
        );
        assert_eq!(kind_for_mime("application/json", 10), Some(Kind::Html));
        assert_eq!(kind_for_mime("application/json", HTML_MAX_SIZE + 1), None);
        assert_eq!(
            kind_for_mime("text/html", HTML_MAX_SIZE + 1),
            Some(Kind::Html)
        );
        assert_eq!(
            kind_for_mime("application/x-sqlite3", 10),
            Some(Kind::Sqlite)
        );
        assert_eq!(kind_for_mime("application/pdf", 10), Some(Kind::Pdf));
        assert_eq!(kind_for_mime("application/msword", 10), Some(Kind::Ole));
        assert_eq!(
            kind_for_mime(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                10
            ),
            Some(Kind::Ooxml)
        );
        assert_eq!(
            kind_for_mime("application/vnd.oasis.opendocument.text", 10),
            Some(Kind::Odf)
        );
        assert_eq!(kind_for_mime("message/rfc822", 10), Some(Kind::Email));
        assert_eq!(kind_for_mime("text/csv", 10), Some(Kind::Plain));
        assert_eq!(kind_for_mime("image/svg+xml", 10), Some(Kind::Xml));
        for none in [
            OCTET_STREAM,
            "application/x-msdownload",
            "application/zip",
            "application/x-executable",
            "video/mp4",
            "image/jpeg",
        ] {
            assert_eq!(kind_for_mime(none, 10), None, "{none}");
        }
        assert!(Kind::Pdf.is_document());
        assert!(!Kind::Plain.is_document());
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff_mime(b"%PDF-1.7", "a"), "application/pdf");
        assert_eq!(sniff_mime(b"{\\rtf1\\ansi", "a"), "application/rtf");
        assert_eq!(
            sniff_mime(b"SQLite format 3\0", "a"),
            "application/x-sqlite3"
        );
        assert_eq!(
            sniff_mime(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1], "x.doc"),
            "application/msword"
        );
        assert_eq!(sniff_mime(b"hello world\n", "a.txt"), "text/plain");
        assert_eq!(sniff_mime(b"  <!DOCTYPE html><html>", "a"), "text/html");
        assert_eq!(
            sniff_mime(b"<?xml version=\"1.0\"?><a/>", "a"),
            "application/xml"
        );
        assert_eq!(
            sniff_mime(b"From: a@b.c\nTo: d@e.f\nSubject: hi\n\nbody", "m"),
            "message/rfc822"
        );
        assert_eq!(sniff_mime(b"\x00\x01\x02binary", "a"), OCTET_STREAM);
        assert_eq!(sniff_mime(b"", "a"), OCTET_STREAM);
        assert_eq!(sniff_mime(b"\x7FELF\x02", "a"), "application/x-executable");
        let mut odt = b"PK\x03\x04".to_vec();
        odt.extend([0u8; 22]);
        odt.extend(8u16.to_le_bytes());
        odt.extend(0u16.to_le_bytes());
        odt.extend(b"mimetypeapplication/vnd.oasis.opendocument.text");
        assert_eq!(
            sniff_mime(&odt, "noext"),
            "application/vnd.oasis.opendocument.text"
        );
        assert_eq!(
            sniff_mime(b"PK\x03\x04", "a.docx"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
    }
}

//! Autopsy's `FileTypeDetector` (`modules/filetypeid`): its own
//! signatures and user-defined types ahead of Tika, plus its fixes to
//! Tika's answer.

use pm_core::ReadAt;
use serde::{Deserialize, Serialize};

use crate::db::MimeDb;
use crate::media;
use crate::text;
use crate::tika::{self, Containers};
use crate::xml::{Event, Reader};

/// Bytes read for signature matching (`defaultBufferSize`).
const SIGNATURE_BUFFER: usize = 600;
/// Slack files smaller than this are not typed.
const SLACK_FILE_THRESHOLD: u64 = 4096;

/// How a signature's bytes were entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SignatureKind {
    Raw,
    Ascii,
}

/// One signature of a custom file type (Autopsy `FileType.Signature`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// The bytes, as lower-case hex in settings.
    #[serde(with = "hex_bytes")]
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub kind: SignatureKind,
    /// The offset counts from the start (otherwise back from the last
    /// byte).
    #[serde(default = "yes")]
    pub relative_to_start: bool,
}

fn yes() -> bool {
    true
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&pm_core::decode::to_hex(b))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        super::decode_hex(&s).ok_or_else(|| serde::de::Error::custom("invalid hex"))
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| {
            s.get(i * 2..i * 2 + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        })
        .collect()
}

impl Signature {
    /// A raw (hex) signature from the start of the file.
    pub fn raw(bytes: &[u8], offset: u64) -> Self {
        Self {
            bytes: bytes.to_vec(),
            offset,
            kind: SignatureKind::Raw,
            relative_to_start: true,
        }
    }

    /// An ASCII signature from the start of the file.
    pub fn ascii(s: &str, offset: u64) -> Self {
        Self {
            bytes: s
                .bytes()
                .map(|b| if b.is_ascii() { b } else { b'?' })
                .collect(),
            offset,
            kind: SignatureKind::Ascii,
            relative_to_start: true,
        }
    }

    /// Counts the offset back from the last byte.
    #[must_use]
    pub fn from_end(mut self) -> Self {
        self.relative_to_start = false;
        self
    }

    /// `Signature.containedIn`.
    pub fn contained_in(&self, src: &dyn ReadAt, head: &[u8]) -> bool {
        let size = src.size();
        if self.offset >= size {
            return false;
        }
        let at = if self.relative_to_start {
            self.offset
        } else {
            size - 1 - self.offset
        };
        let len = self.bytes.len() as u64;
        if size < at.saturating_add(len) {
            return false;
        }
        let end = at + len;
        if let Some(b) = usize::try_from(at)
            .ok()
            .zip(usize::try_from(end).ok())
            .and_then(|(a, e)| head.get(a..e))
        {
            return b == self.bytes.as_slice();
        }
        let mut buf = vec![0u8; self.bytes.len()];
        matches!(src.read_exact_at(at, &mut buf), Ok(())) && buf == self.bytes
    }
}

/// Invalid custom file types.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TypeError {
    #[error("a file type needs a MIME type")]
    NoMimeType,
    #[error("file type {0} has no signatures")]
    NoSignatures(String),
    #[error("file type {0} has an empty signature")]
    EmptySignature(String),
    #[error("file type {0} creates interesting-file hits but has no set name")]
    NoSetName(String),
    #[error("invalid file type definitions: {0}")]
    Xml(String),
}

/// A custom (user- or Autopsy-defined) file type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileType {
    pub mime_type: String,
    pub signatures: Vec<Signature>,
    /// Post an interesting-item result for files of this type.
    #[serde(default)]
    pub create_interesting_file_hit: bool,
    #[serde(default)]
    pub interesting_files_set_name: String,
}

impl FileType {
    /// A type without interesting-file hits.
    pub fn new(mime_type: &str, signatures: Vec<Signature>) -> Self {
        Self {
            mime_type: mime_type.to_owned(),
            signatures,
            create_interesting_file_hit: false,
            interesting_files_set_name: String::new(),
        }
    }

    /// Checks the definition (Autopsy's editor and constructor rules).
    pub fn validate(&self) -> Result<(), TypeError> {
        if self.mime_type.trim().is_empty() {
            return Err(TypeError::NoMimeType);
        }
        if self.signatures.is_empty() {
            return Err(TypeError::NoSignatures(self.mime_type.clone()));
        }
        if self.signatures.iter().any(|s| s.bytes.is_empty()) {
            return Err(TypeError::EmptySignature(self.mime_type.clone()));
        }
        if self.create_interesting_file_hit && self.interesting_files_set_name.trim().is_empty() {
            return Err(TypeError::NoSetName(self.mime_type.clone()));
        }
        Ok(())
    }

    /// All signatures match (`FileType.matches`).
    pub fn matches(&self, src: &dyn ReadAt, head: &[u8]) -> bool {
        self.signatures.iter().all(|s| s.contained_in(src, head))
    }
}

/// Imports Autopsy's legacy `UserFileTypeDefinitions.xml` (one signature
/// per type; no interesting-file options).
pub fn import_legacy_xml(xml: &str) -> Result<Vec<FileType>, TypeError> {
    let bad = |m: &str| TypeError::Xml(m.to_owned());
    let mut r = Reader::new(xml, false, false).with_text();
    let mut out = Vec::new();
    let mut root_seen = false;
    let mut cur_mime: Option<String> = None;
    let mut sig_kind: Option<SignatureKind> = None;
    let mut bytes: Option<Vec<u8>> = None;
    let mut offset: Option<u64> = None;
    let mut rel = true;
    let mut text_target: Option<&'static str> = None;
    let mut text = String::new();
    let mut in_type = false;
    loop {
        match r.next() {
            Ok(None) => break,
            Err(_) => return Err(bad("not well-formed")),
            Ok(Some(Event::Start(t))) => {
                if !root_seen {
                    root_seen = true;
                    if t.qname != "FileTypes" {
                        return Ok(Vec::new());
                    }
                    continue;
                }
                match t.qname.as_str() {
                    "FileType" => {
                        in_type = true;
                        cur_mime = None;
                        sig_kind = None;
                        bytes = None;
                        offset = None;
                        rel = true;
                    }
                    "MimeType" if in_type && cur_mime.is_none() => text_target = Some("mime"),
                    "Signature" if in_type && sig_kind.is_none() => {
                        sig_kind = Some(match t.attr("type") {
                            Some("RAW") => SignatureKind::Raw,
                            Some("ASCII") => SignatureKind::Ascii,
                            _ => return Err(bad("signature type")),
                        });
                    }
                    "Bytes" if in_type && bytes.is_none() => text_target = Some("bytes"),
                    "Offset" if in_type && offset.is_none() => {
                        rel = match t.attr("RelativeToStart") {
                            None | Some("") => true,
                            Some(v) => v.eq_ignore_ascii_case("true"),
                        };
                        text_target = Some("offset");
                    }
                    _ => {}
                }
                text.clear();
            }
            Ok(Some(Event::Text(s))) => {
                if text_target.is_some() {
                    text.push_str(&s);
                }
            }
            Ok(Some(Event::End(name))) => {
                match (text_target, name.as_str()) {
                    (Some("mime"), "MimeType") => cur_mime = Some(text.clone()),
                    (Some("bytes"), "Bytes") => {
                        bytes = Some(decode_hex(&text).ok_or_else(|| bad("signature bytes"))?);
                    }
                    (Some("offset"), "Offset") => {
                        offset = Some(text.trim().parse().map_err(|_| bad("offset"))?);
                    }
                    _ => {}
                }
                if matches!(name.as_str(), "MimeType" | "Bytes" | "Offset") {
                    text_target = None;
                }
                if name == "FileType" {
                    in_type = false;
                    let sig = Signature {
                        bytes: bytes.take().ok_or_else(|| bad("signature bytes"))?,
                        offset: offset.take().ok_or_else(|| bad("offset"))?,
                        kind: sig_kind.take().ok_or_else(|| bad("signature"))?,
                        relative_to_start: rel,
                    };
                    let t = FileType::new(&cur_mime.take().unwrap_or_default(), vec![sig]);
                    out.push(t);
                }
            }
        }
    }
    Ok(out)
}

/// Autopsy's built-in signatures, in its order
/// (`CustomFileTypesManager.createAutopsyDefinedFileTypes`).
pub fn autopsy_defined_types() -> Vec<FileType> {
    let raw = |b: &[u8], o: u64| Signature::raw(b, o);
    let asc = |s: &str, o: u64| Signature::ascii(s, o);
    let t = |m: &str, s: Vec<Signature>| FileType::new(m, s);
    vec![
        t("text/xml", vec![asc("<?xml", 0)]),
        t("application/x-gzip", vec![raw(&[0x1F, 0x8B], 0)]),
        t(
            "application/x-123",
            vec![raw(&[0, 0, 2, 0, 6, 4, 6, 0, 8, 0, 0, 0, 0, 0], 0)],
        ),
        t("image/vnd.radiance", vec![raw(b"#?RADIANCE\n", 0)]),
        t("image/x-dcx", vec![raw(&[0xB1, 0x68, 0xDE, 0x3A], 0)]),
        t("image/x-icns", vec![asc("icns", 0)]),
        t("image/x-pict", vec![raw(&[0x00, 0x11, 0x02, 0xFF], 522)]),
        t("image/x-portable-arbitrarymap", vec![asc("P7", 0)]),
        t("image/x-portable-floatmap", vec![asc("PF", 0)]),
        t("image/x-portable-floatmap", vec![asc("Pf", 0)]),
        t(
            "image/x-tga",
            vec![raw(b"TRUEVISION-XFILE.\0", 17).from_end()],
        ),
        t("image/x-ilbm", vec![asc("FORM", 0), asc("ILBM", 8)]),
        t("image/x-ilbm", vec![asc("FORM", 0), asc("PBM", 8)]),
        t("image/webp", vec![asc("RIFF", 0), asc("WEBP", 8)]),
        t("audio/aiff", vec![asc("FORM", 0), asc("AIFF", 8)]),
        t("audio/aiff", vec![asc("FORM", 0), asc("AIFC", 8)]),
        t("audio/aiff", vec![asc("FORM", 0), asc("8SVX", 8)]),
        t("application/x-iff", vec![asc("FORM", 0)]),
        t("image/jpeg", vec![raw(&[0xFF, 0xD9, 0xFF, 0xD8], 0)]),
        t("application/x.windows-registry", vec![raw(b"regf", 0)]),
        t("application/x.android-hdb", vec![raw(&[0xCC; 24], 8)]),
        t("application/x-vhd", vec![asc("conectix", 511).from_end()]),
    ]
}

/// What Autopsy knows about a file before reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    /// A regular file (`isFile()`), including carved and derived files.
    File,
    /// A slack-space file.
    Slack,
    /// Unallocated or unused blocks.
    Unallocated,
    /// Directories, virtual directories and other non-file objects.
    Other,
}

/// Autopsy's MIME detector: user types, Autopsy types, then Tika.
#[derive(Debug, Clone)]
pub struct FileTypeDetector {
    user_types: Vec<FileType>,
    autopsy_types: Vec<FileType>,
}

impl Default for FileTypeDetector {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// `removeOptionalParameter`.
pub fn strip_params(m: &str) -> &str {
    match m.find(';') {
        Some(i) => m.get(..i).unwrap_or(m).trim(),
        None => m,
    }
}

/// Autopsy's clean-up of a Tika type: no `tika-`, no parameters.
fn clean(t: &str) -> String {
    strip_params(&t.replace("tika-", "")).to_owned()
}

impl FileTypeDetector {
    /// A detector with the given user-defined types (checked in order).
    pub fn new(user_types: Vec<FileType>) -> Self {
        Self {
            user_types,
            autopsy_types: autopsy_defined_types(),
        }
    }

    /// The user-defined types.
    pub fn user_types(&self) -> &[FileType] {
        &self.user_types
    }

    /// The first user-defined type `src` matches (`detectUserDefinedType`).
    pub fn user_type_of(&self, src: &dyn ReadAt, head: &[u8]) -> Option<&FileType> {
        self.user_types.iter().find(|t| t.matches(src, head))
    }

    /// `FileTypeDetector.getMIMEType` for a file without a stored type.
    /// `extension` is the lower-case name extension.
    pub fn detect(
        &self,
        class: FileClass,
        name: &str,
        extension: &str,
        src: &dyn ReadAt,
    ) -> String {
        let size = src.size();
        let not_typed = match class {
            FileClass::File => size == 0,
            FileClass::Slack => size < SLACK_FILE_THRESHOLD,
            FileClass::Unallocated | FileClass::Other => true,
        };
        if not_typed {
            return media::OCTET_STREAM.to_owned();
        }
        let n = usize::try_from(size.min(SIGNATURE_BUFFER as u64)).unwrap_or(0);
        let mut head = vec![0u8; n];
        let got = src.read_at(0, &mut head).unwrap_or(0);
        head.truncate(got);
        if let Some(t) = self.user_type_of(src, &head) {
            return t.mime_type.clone();
        }
        if let Some(t) = self.autopsy_types.iter().find(|t| t.matches(src, &head)) {
            return t.mime_type.clone();
        }
        Self::detect_tika(name, extension, src)
    }

    /// The Tika part: pass 1 without the name; if that names a type, pass
    /// 2 with the name; `.txt` charset fallback; the `audio/mpeg` fix.
    fn detect_tika(name: &str, extension: &str, src: &dyn ReadAt) -> String {
        let db = MimeDb::get();
        let prefix = tika::read_prefix(src);
        let Ok(containers) = Containers::run(src, &prefix) else {
            return media::OCTET_STREAM.to_owned();
        };
        let first = clean(&tika::combine(
            db,
            &containers,
            &tika::mime_types(db, &prefix, None),
        ));
        let mut mime = if first == media::OCTET_STREAM {
            if extension == "txt" && text::has_text_encoding(&prefix) {
                media::TEXT_PLAIN.to_owned()
            } else {
                first
            }
        } else {
            clean(&tika::combine(
                db,
                &containers,
                &tika::mime_types(db, &prefix, Some(name)),
            ))
        };
        if mime.contains("audio/mpeg") && prefix.starts_with(&[0xFF, 0xFF]) {
            media::OCTET_STREAM.clone_into(&mut mime);
        }
        mime
    }

    /// Whether `mime` is a type the detector can produce
    /// (`FileTypeDetector.isDetectable`).
    pub fn is_detectable(&self, mime: &str) -> bool {
        self.user_types.iter().any(|t| t.mime_type == mime)
            || self.autopsy_types.iter().any(|t| t.mime_type == mime)
            || tika_detectable_types()
                .iter()
                .any(|t| t == strip_params(mime))
    }
}

/// Tika's registered types without parameters, `tika-` removed
/// (`getTikaDetectedTypes`).
pub fn tika_detectable_types() -> Vec<String> {
    let mut v: Vec<String> = MimeDb::get()
        .types()
        .into_iter()
        .filter(|t| !media::has_params(t))
        .map(|t| t.replace("tika-", ""))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Every type the detector can produce, sorted case-insensitively
/// (`FileTypeDetector.getDetectedTypes`).
pub fn detected_types(user_types: &[FileType]) -> Vec<String> {
    let mut v = tika_detectable_types();
    v.extend(autopsy_defined_types().into_iter().map(|t| t.mime_type));
    v.extend(user_types.iter().map(|t| t.mime_type.clone()));
    v.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    fn det(d: &FileTypeDetector, data: &[u8], name: &str) -> String {
        let ext = name.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
        d.detect(FileClass::File, name, &ext, &MemSource::new(data.to_vec()))
    }

    #[test]
    fn special_cases() {
        let d = FileTypeDetector::default();
        let src = MemSource::new(b"%PDF-1.4".to_vec());
        assert_eq!(
            d.detect(FileClass::Other, "d", "", &src),
            "application/octet-stream"
        );
        assert_eq!(
            d.detect(FileClass::Unallocated, "u", "", &src),
            "application/octet-stream"
        );
        assert_eq!(
            d.detect(FileClass::Slack, "s", "", &src),
            "application/octet-stream"
        );
        assert_eq!(det(&d, b"", "e.pdf"), "application/octet-stream");
        assert_eq!(det(&d, b"%PDF-1.4\n", "a.pdf"), "application/pdf");
        // Autopsy-defined types win over Tika.
        assert_eq!(
            det(&d, b"<?xml version=\"1.0\"?><svg/>", "x.svg"),
            "text/xml"
        );
        assert_eq!(
            det(&d, &[0x1F, 0x8B, 8, 0, 0, 0], "a.gz"),
            "application/x-gzip"
        );
        assert_eq!(
            det(&d, b"regf....", "NTUSER.DAT"),
            "application/x.windows-registry"
        );
        let mut tga = vec![0u8; 40];
        tga.extend_from_slice(b"TRUEVISION-XFILE.\0");
        assert_eq!(det(&d, &tga, "a.tga"), "image/x-tga");
        // audio/mpeg starting with FF FF is not MP3.
        assert_eq!(
            det(&d, &[0xFF, 0xFF, 0xFF, 0xFB, 0x90, 0], "x.mp3"),
            "application/octet-stream"
        );
        // Binary .txt with a legacy encoding.
        assert_eq!(det(&d, &[0xE4u8; 64], "x.txt"), "text/plain");
        assert_eq!(det(&d, &[0xE4u8; 64], "x.bin"), "application/octet-stream");
    }

    #[test]
    fn user_types_first() {
        let mut t = FileType::new(
            "application/x-mine",
            vec![
                Signature::ascii("MINE", 0),
                Signature::raw(&[0xAA], 0).from_end(),
            ],
        );
        t.create_interesting_file_hit = true;
        t.interesting_files_set_name = "Mine".into();
        assert!(t.validate().is_ok());
        let d = FileTypeDetector::new(vec![t]);
        assert_eq!(det(&d, b"MINE...\xAA", "a"), "application/x-mine");
        assert_eq!(det(&d, b"MINE...\xAB", "a.txt"), "text/plain");
        assert!(d.is_detectable("application/x-mine"));
        assert!(d.is_detectable("application/x-msoffice"));
        assert!(d.is_detectable("text/plain; charset=x"));
        assert!(!d.is_detectable("application/x-tika-msoffice"));
        let bad = FileType::new("", vec![]);
        assert_eq!(bad.validate(), Err(TypeError::NoMimeType));
    }

    #[test]
    fn legacy_xml_and_serde() {
        let xml = r#"<?xml version="1.0"?><FileTypes><FileType><MimeType>application/x-a</MimeType><Signature type="RAW"><Bytes>414243</Bytes><Offset RelativeToStart="false">4</Offset></Signature></FileType><FileType><MimeType>text/x-b</MimeType><Signature type="ASCII"><Bytes>78</Bytes><Offset>0</Offset></Signature></FileType></FileTypes>"#;
        let types = import_legacy_xml(xml).unwrap();
        assert_eq!(types.len(), 2);
        assert_eq!(types[0].signatures[0].bytes, b"ABC");
        assert!(!types[0].signatures[0].relative_to_start);
        assert_eq!(types[1].signatures[0].kind, SignatureKind::Ascii);
        let json = serde_json::to_string(&types).unwrap();
        assert!(json.contains("\"414243\""));
        let back: Vec<FileType> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, types);
    }
}

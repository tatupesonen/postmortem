//! Tika's `DefaultZipContainerDetector` on a stream that is not backed by
//! a file (Autopsy's case): archive/compressor sniffing of the first
//! 1 KiB, then a streaming walk over the local file headers (Commons
//! Compress `ZipArchiveInputStream` semantics, at most 16 MiB) that feeds
//! Tika's eight `ZipContainerDetector`s in tika-app's order: iWork, IPA,
//! JAR/APK/WAR/EAR, KMZ, `OpenDocument`, `StarOffice`, Frictionless, OOXML.

use flate2::{Decompress, FlushDecompress, Status};
use pm_core::ReadAt;

use crate::archive::{self, Archive};
use crate::media;
use crate::stream::SeqReader;
use crate::xml;

/// `DefaultZipContainerDetector.markLimit`.
const MARK_LIMIT: u64 = 16 * 1024 * 1024;
const LFH: &[u8; 4] = b"PK\x03\x04";
const CFH: &[u8; 4] = b"PK\x01\x02";
const DD: &[u8; 4] = b"PK\x07\x08";
const SPLIT: &[u8; 4] = b"PK00";
const EOCD: &[u8; 4] = b"PK\x05\x06";

const STORED: u16 = 0;
const DEFLATED: u16 = 8;
const DEFLATE64: u16 = 9;
const BZIP2: u16 = 12;
const UNSHRINKING: u16 = 1;
const IMPLODING: u16 = 6;

/// Errors of the streaming reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZipErr {
    /// `UnsupportedZipFeatureException(DATA_DESCRIPTOR)`: retried with
    /// stored entries allowed.
    DataDescriptor,
    /// `UnsupportedZipFeatureException(ENCRYPTION)`.
    Encrypted,
    /// Any other I/O or format error.
    Other,
}

/// A local file header.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    flags: u16,
    method: u16,
    csize: Option<u64>,
    size: Option<u64>,
    data_start: u64,
    zip64: bool,
}

impl Entry {
    fn has_dd(&self) -> bool {
        self.flags & 0x08 != 0
    }

    fn encrypted(&self) -> bool {
        self.flags & 0x01 != 0
    }
}

/// `ZipArchiveInputStream` over the first 16 MiB.
struct Walker<'a> {
    r: SeqReader<'a>,
    allow_stored_dd: bool,
    first: bool,
    ended: bool,
    current: Option<Entry>,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u16::from_le_bytes)
}

fn u32le(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

fn u64le(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

impl<'a> Walker<'a> {
    fn new(src: &'a dyn ReadAt, allow_stored_dd: bool) -> Self {
        Self {
            r: SeqReader::bounded(src, 0, MARK_LIMIT),
            allow_stored_dd,
            first: true,
            ended: false,
            current: None,
        }
    }

    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, ZipErr> {
        let v = self.r.take(n);
        if v.len() == n {
            Ok(v)
        } else {
            Err(ZipErr::Other)
        }
    }

    /// `readFirstLocalFileHeader` (the archive sniffing already found a
    /// signature at offset 0).
    fn first_header(&mut self) -> Result<Option<Vec<u8>>, ZipErr> {
        let h = self.read_exact(22)?;
        let sig = h.get(..4).unwrap_or_default();
        if sig == EOCD {
            return Ok(None);
        }
        if sig == DD {
            // Split archives are not supported.
            return Err(ZipErr::Other);
        }
        if sig != LFH && sig != SPLIT {
            return Err(ZipErr::Other);
        }
        let mut lfh = h;
        lfh.extend(self.read_exact(8)?);
        if lfh.get(..4) == Some(&SPLIT[..]) {
            let mut rest = lfh.get(4..).unwrap_or_default().to_vec();
            rest.extend(self.read_exact(4)?);
            lfh = rest;
        }
        Ok(Some(lfh))
    }

    /// `getNextZipEntry`.
    fn next_entry(&mut self) -> Result<Option<Entry>, ZipErr> {
        if self.ended {
            return Ok(None);
        }
        let lfh = if self.first {
            self.first = false;
            let Some(h) = self.first_header()? else {
                self.ended = true;
                return Ok(None);
            };
            h
        } else {
            self.close_entry()?;
            let h = self.r.take(30);
            if h.len() < 30 {
                return Ok(None);
            }
            h
        };
        let sig = lfh.get(..4).unwrap_or_default();
        if sig != LFH {
            self.ended = true;
            // The central directory (or an APK signing block): the end.
            // Anything else is a ZipException; both stop the walk.
            return if sig == CFH || sig == b"PK\x06\x08" {
                Ok(None)
            } else {
                Err(ZipErr::Other)
            };
        }
        let flags = u16le(&lfh, 6);
        let method = u16le(&lfh, 8);
        let has_dd = flags & 0x08 != 0;
        let (mut csize, mut size) = if has_dd {
            (None, None)
        } else {
            (
                Some(u64::from(u32le(&lfh, 18))),
                Some(u64::from(u32le(&lfh, 22))),
            )
        };
        let name_len = usize::from(u16le(&lfh, 26));
        let extra_len = usize::from(u16le(&lfh, 28));
        let name = self.read_exact(name_len)?;
        let extra = self.read_exact(extra_len)?;
        // Zip64 extended information.
        let mut zip64 = false;
        let mut o = 0;
        while o + 4 <= extra.len() {
            let id = u16le(&extra, o);
            let len = usize::from(u16le(&extra, o + 2));
            if id == 0x0001 {
                zip64 = true;
                let body = extra.get(o + 4..o + 4 + len).unwrap_or_default();
                let mut p = 0;
                if (size == Some(0xFFFF_FFFF) || csize == Some(0xFFFF_FFFF)) && body.len() >= 16 {
                    {
                        size = Some(u64le(body, p));
                        p += 8;
                        csize = Some(u64le(body, p));
                    }
                }
                break;
            }
            o += 4 + len;
        }
        let e = Entry {
            name: String::from_utf8_lossy(&name).into_owned(),
            flags,
            method,
            csize,
            size,
            data_start: self.r.position(),
            zip64,
        };
        // A BZip2 entry's stream is opened (and its header checked) now.
        if e.method == BZIP2 && e.csize.is_some() {
            let pos = self.r.position();
            let head = self.r.take(10);
            self.r.seek(pos);
            let ok = head.len() == 10
                && head.starts_with(b"BZh")
                && head.get(3).is_some_and(|d| (b'1'..=b'9').contains(d))
                && matches!(
                    head.get(4..10),
                    Some(
                        [0x31, 0x41, 0x59, 0x26, 0x53, 0x59] | [0x17, 0x72, 0x45, 0x38, 0x50, 0x90]
                    )
                );
            if !ok {
                return Err(ZipErr::Other);
            }
        }
        self.current = Some(e.clone());
        Ok(Some(e))
    }

    /// `ZipUtil.checkRequestedFeatures` + the data-descriptor checks done
    /// by `read()`.
    fn check_readable(&self, e: &Entry) -> Result<(), ZipErr> {
        if e.encrypted() {
            return Err(ZipErr::Encrypted);
        }
        if !matches!(
            e.method,
            STORED | UNSHRINKING | IMPLODING | DEFLATED | DEFLATE64 | BZIP2
        ) {
            return Err(ZipErr::Other);
        }
        if e.has_dd()
            && !((self.allow_stored_dd && e.method == STORED)
                || e.method == DEFLATED
                || e.method == DEFLATE64)
        {
            return Err(ZipErr::DataDescriptor);
        }
        Ok(())
    }

    /// Inflates from `start`: up to `max` output bytes; returns the output
    /// and, when the stream ended, the compressed bytes consumed.
    fn inflate(&mut self, start: u64, max: usize) -> Result<(Vec<u8>, Option<u64>), ZipErr> {
        self.r.seek(start);
        let mut d = Decompress::new(false);
        let mut out: Vec<u8> = Vec::new();
        let mut inbuf = vec![0u8; 16 * 1024];
        let mut have = 0usize;
        let mut used = 0usize;
        let mut obuf = vec![0u8; 32 * 1024];
        loop {
            if used == have {
                have = self.r.read(&mut inbuf);
                used = 0;
                if have == 0 {
                    // "Truncated ZIP file".
                    return Err(ZipErr::Other);
                }
            }
            let before_in = d.total_in();
            let before_out = d.total_out();
            let status = d
                .decompress(
                    inbuf.get(used..have).unwrap_or_default(),
                    &mut obuf,
                    FlushDecompress::None,
                )
                .map_err(|_| ZipErr::Other)?;
            used += usize::try_from(d.total_in() - before_in).unwrap_or(0);
            let produced = usize::try_from(d.total_out() - before_out).unwrap_or(0);
            if out.len() < max {
                let keep = produced.min(max - out.len());
                out.extend_from_slice(obuf.get(..keep).unwrap_or_default());
            }
            if status == Status::StreamEnd {
                return Ok((out, Some(d.total_in())));
            }
            if out.len() >= max {
                return Ok((out, None));
            }
            if produced == 0 && d.total_in() == before_in && used < have {
                return Err(ZipErr::Other);
            }
        }
    }

    /// `readDataDescriptor` at the current position.
    fn data_descriptor(&mut self, e: &Entry) -> Result<(u64, u64), ZipErr> {
        let w = self.read_exact(4)?;
        if w == DD {
            self.read_exact(4)?;
        }
        let pos = self.r.position();
        let two = self.read_exact(16)?;
        let potential = two.get(8..12).unwrap_or_default();
        let _ = e;
        if potential == CFH || potential == LFH {
            self.r.seek(pos + 8);
            Ok((u64::from(u32le(&two, 0)), u64::from(u32le(&two, 4))))
        } else {
            let c = u64le(&two, 0);
            let s = u64le(&two, 8);
            if c > i64::MAX as u64 || s > i64::MAX as u64 {
                return Err(ZipErr::Other);
            }
            Ok((c, s))
        }
    }

    /// `readStoredEntry`: scans for the next signature after stored data
    /// of unknown size.
    fn stored_dd_end(&mut self, e: &Entry) -> Result<u64, ZipErr> {
        let dd_len: u64 = if e.zip64 { 20 } else { 12 };
        self.r.seek(e.data_start);
        let mut window: Vec<u8> = Vec::new();
        let mut base = e.data_start;
        loop {
            let chunk = self.r.take(64 * 1024);
            if chunk.is_empty() {
                return Err(ZipErr::Other);
            }
            window.extend_from_slice(&chunk);
            let mut i = 0usize;
            while i + 4 <= window.len() {
                if window.get(i..i + 2) == Some(b"PK") {
                    let k = window.get(i + 2..i + 4).unwrap_or_default();
                    let abs = base + i as u64;
                    let rel = abs - e.data_start;
                    if (rel >= dd_len && k == b"\x03\x04") || k == b"\x01\x02" {
                        return rel.checked_sub(dd_len).ok_or(ZipErr::Other);
                    }
                    if k == b"\x07\x08" {
                        return Ok(rel);
                    }
                }
                i += 1;
            }
            // Keep the last 3 bytes for signatures across chunks.
            let keep = window.len().min(3);
            base += (window.len() - keep) as u64;
            window.drain(..window.len() - keep);
        }
    }

    /// `closeEntry`: positions after the current entry's data.
    fn close_entry(&mut self) -> Result<(), ZipErr> {
        let Some(e) = self.current.take() else {
            return Ok(());
        };
        if !e.has_dd() {
            let end = e
                .data_start
                .checked_add(e.csize.unwrap_or(0))
                .ok_or(ZipErr::Other)?;
            self.r.seek(e.data_start);
            if !self.r.skip(end - e.data_start) {
                return Err(ZipErr::Other);
            }
            return Ok(());
        }
        self.check_readable(&e)?;
        match e.method {
            STORED => {
                let len = self.stored_dd_end(&e)?;
                self.r.seek(e.data_start + len);
                let (c, s) = self.data_descriptor(&e)?;
                if c != s || len != s {
                    return Err(ZipErr::Other);
                }
            }
            DEFLATED => {
                let (_, consumed) = self.inflate(e.data_start, 0)?;
                let used = consumed.ok_or(ZipErr::Other)?;
                self.r.seek(e.data_start + used);
                self.data_descriptor(&e)?;
            }
            // Deflate64 streams are not decoded.
            _ => return Err(ZipErr::Other),
        }
        Ok(())
    }

    /// Reads up to `max` bytes of the current entry's content. On a read
    /// error, returns what was read before it (SAX parsers see that much).
    fn content(&mut self, e: &Entry, max: usize) -> (Vec<u8>, Option<ZipErr>) {
        if let Err(err) = self.check_readable(e) {
            return (Vec::new(), Some(err));
        }
        let r = match e.method {
            STORED if !e.has_dd() => {
                let n = e.size.unwrap_or(0).min(max as u64);
                self.r.seek(e.data_start);
                let v = self.r.take(usize::try_from(n).unwrap_or(0));
                let short = (v.len() as u64) < n;
                return (v, short.then_some(ZipErr::Other));
            }
            STORED => self.stored_dd_end(e).map(|len| {
                self.r.seek(e.data_start);
                let n = usize::try_from(len.min(max as u64)).unwrap_or(0);
                self.r.take(n)
            }),
            DEFLATED => self.inflate(e.data_start, max).map(|(v, _)| v),
            _ => Err(ZipErr::Other),
        };
        match r {
            Ok(v) => (v, None),
            Err(err) => (Vec::new(), Some(err)),
        }
    }
}

/// Per-walk state of the zip detectors (`StreamingDetectContext`).
#[derive(Debug, Default)]
struct Ctx {
    seen_any: bool,
    iwork13_main: bool,
    iwork_common: bool,
    ipa_left: Option<[bool; 6]>,
    jar_manifest: bool,
    kml: Option<usize>,
    /// Frictionless parts: package, archive/, indexes/, pages/.
    parts: Option<[bool; 4]>,
    ooxml_hints: Option<usize>,
}

const IPA_PATTERNS: [fn(&str) -> bool; 6] = [
    |n| n == "Payload/",
    |n| ipa_app(n, "/"),
    |n| ipa_app(n, "/_CodeSignature/"),
    |n| ipa_app(n, "/_CodeSignature/CodeResources"),
    |n| ipa_app(n, "/Info.plist"),
    |n| ipa_app(n, "/PkgInfo"),
];

/// `^Payload/.*\.app<suffix>$` (`.` excludes line terminators).
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Tika's patterns are case-sensitive"
)]
fn ipa_app(n: &str, suffix: &str) -> bool {
    let Some(rest) = n.strip_prefix("Payload/") else {
        return false;
    };
    let Some(mid) = rest.strip_suffix(suffix) else {
        return false;
    };
    mid.ends_with(".app") && !mid.contains(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}'])
}

const OOXML_HINTS: [&str; 8] = [
    "word/document.xml",
    "_rels/.rels",
    "[Content_Types].xml",
    "ppt/presentation.xml",
    "ppt/slides/slide1.xml",
    "xl/workbook.xml",
    "xl/sharedStrings.xml",
    "xl/worksheets/sheet1.xml",
];

/// `OPCPackageDetector.OOXML_CONTENT_TYPES` (POI 5.3 relation types).
fn ooxml_content_type(ct: &str) -> Option<&'static str> {
    Some(match ct {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        }
        "application/vnd.ms-word.document.macroEnabled.main+xml" => {
            "application/vnd.ms-word.document.macroenabled.12"
        }
        "application/vnd.ms-word.template.macroEnabledTemplate.main+xml" => {
            "application/vnd.ms-word.template.macroenabled.12"
        }
        "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.template"
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml" => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        }
        "application/vnd.ms-excel.sheet.macroEnabled.main+xml" => {
            "application/vnd.ms-excel.sheet.macroenabled.12"
        }
        "application/vnd.ms-excel.sheet.binary.macroEnabled.main" => {
            "application/vnd.ms-excel.sheet.binary.macroenabled.12"
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml" => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.template"
        }
        "application/vnd.ms-excel.template.macroEnabled.main+xml" => {
            "application/vnd.ms-excel.template.macroenabled.12"
        }
        "application/vnd.ms-excel.addin.macroEnabled.main+xml" => {
            "application/vnd.ms-excel.addin.macroenabled.12"
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        }
        "application/vnd.ms-powerpoint.slideshow.macroEnabled.main+xml" => {
            "application/vnd.ms-powerpoint.slideshow.macroenabled.12"
        }
        "application/vnd.ms-powerpoint.template.macroEnabled.main+xml" => {
            "application/vnd.ms-powerpoint.template.macroenabled.12"
        }
        // PRESENTATIONML_TEMPLATE is put twice; the second (POTX) wins.
        "application/vnd.openxmlformats-officedocument.presentationml.template.main+xml" => {
            "application/vnd.openxmlformats-officedocument.presentationml.template"
        }
        "application/vnd.openxmlformats-officedocument.presentationml.slideshow.main+xml" => {
            "application/vnd.openxmlformats-officedocument.presentationml.slideshow"
        }
        "application/vnd.ms-powerpoint.presentation.macroEnabled.main+xml" => {
            "application/vnd.ms-powerpoint.presentation.macroenabled.12"
        }
        "application/vnd.openxmlformats-officedocument.themeManager+xml" => {
            "application/vnd.openxmlformats-officedocument"
        }
        "application/vnd.ms-visio.drawing.macroEnabled.main+xml" => {
            "application/vnd.ms-visio.drawing.macroenabled.12"
        }
        "application/vnd.ms-visio.drawing.main+xml" => "application/vnd.ms-visio.drawing",
        "application/vnd.ms-visio.stencil.macroEnabled.main+xml" => {
            "application/vnd.ms-visio.stencil.macroenabled.12"
        }
        "application/vnd.ms-visio.stencil.main+xml" => "application/vnd.ms-visio.stencil",
        "application/vnd.ms-visio.template.macroEnabled.main+xml" => {
            "application/vnd.ms-visio.template.macroenabled.12"
        }
        "application/vnd.ms-visio.template.main+xml" => "application/vnd.ms-visio.template",
        "application/vnd.ms-package.xps-fixeddocumentsequence+xml" => {
            "application/vnd.ms-xpsdocument"
        }
        _ => return None,
    })
}

const STAR_OFFICE_X: [(&str, &str); 5] = [
    (
        "application/vnd.sun.xml.writer",
        "application/vnd.sun.xml.writer",
    ),
    (
        "application/vnd.sun.xml.calc",
        "application/vnd.sun.xml.calc",
    ),
    (
        "application/vnd.sun.xml.draw",
        "application/vnd.sun.xml.draw",
    ),
    (
        "application/vnd.sun.xml.impress",
        "application/vnd.sun.xml.impress",
    ),
    (
        "application/vnd.sun.star.configuration-data",
        "application/vnd.openofficeorg.extension",
    ),
];

/// `StarOfficeDetector.detectStarOfficeX`.
fn star_office(manifest: &[u8]) -> Option<String> {
    let mut found = None;
    xml::scan_start_tags(manifest, |t| {
        if t.local() != "file-entry" {
            return false;
        }
        let mut media: Option<&str> = None;
        let mut full_path: Option<&str> = None;
        for a in &t.attrs {
            match a.local() {
                "media-type" => {
                    media = Some(&a.value);
                    if let Some((_, m)) = STAR_OFFICE_X.iter().find(|(k, _)| *k == a.value) {
                        found = Some((*m).to_owned());
                        return true;
                    }
                }
                "full-path" => full_path = Some(&a.value),
                _ => {}
            }
        }
        if media == Some("") && full_path == Some("/") {
            found = Some("application/vnd.openofficeorg.autotext".to_owned());
            return true;
        }
        false
    });
    found
}

/// `OPCPackageDetector.parseOOXMLContentTypes`.
fn content_types(data: &[u8]) -> Option<&'static str> {
    let mut found = None;
    xml::scan_start_tags(data, |t| {
        for a in &t.attrs {
            if a.local() == "ContentType"
                && let Some(m) = ooxml_content_type(&a.value)
            {
                found = Some(m);
                return true;
            }
        }
        false
    });
    found
}

/// `IWORKDocumentType.detectType` on the content of an iWork index.
fn iwork_type(data: &[u8]) -> Option<&'static str> {
    let (ns, local) = xml::root_element_stream(data)?;
    match (ns.as_str(), local.as_str()) {
        ("http://developer.apple.com/namespaces/keynote2", "presentation") => {
            Some("application/vnd.apple.keynote")
        }
        ("http://developer.apple.com/namespaces/ls", "document") => {
            Some("application/vnd.apple.numbers")
        }
        ("http://developer.apple.com/namespaces/sl", "document") => {
            Some("application/vnd.apple.pages")
        }
        _ => None,
    }
}

/// Reads limits for parts the detectors parse.
const MAX_MIMETYPE: usize = 1024;
const MAX_MANIFEST: usize = 20 * 1024 * 1024;
const MAX_XML_PART: usize = 16 * 1024 * 1024;

/// Runs every detector's `streamingDetectUpdate` on one entry.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Tika compares names case-sensitively"
)]
fn update(w: &mut Walker<'_>, e: &Entry, ctx: &mut Ctx) -> Result<Option<String>, ZipErr> {
    let name = e.name.as_str();
    // IWorkDetector.
    ctx.seen_any = true;
    if name == "Index/Document.iwa" {
        ctx.iwork13_main = true;
    }
    if name == "buildVersionHistory.plist" {
        ctx.iwork_common = true;
    }
    if matches!(name, "index.apxl" | "index.xml" | "presentation.apxl") {
        let (data, _) = w.content(e, MAX_XML_PART);
        if let Some(t) = iwork_type(&data) {
            return Ok(Some(t.to_owned()));
        }
    }
    for (suffix, t) in [
        (
            ".numbers/Metadata/BuildVersionHistory.plist",
            "application/vnd.apple.numbers.18",
        ),
        (
            ".pages/Metadata/BuildVersionHistory.plist",
            "application/vnd.apple.pages.18",
        ),
        (
            ".key/Metadata/BuildVersionHistory.plist",
            "application/vnd.apple.keynote.18",
        ),
    ] {
        if name.ends_with(suffix) {
            return Ok(Some(t.to_owned()));
        }
    }
    if name.ends_with(".iwa")
        && (name == "Index/MasterSlide.iwa"
            || name.starts_with("Index/MasterSlide-")
            || name == "Index/Slide.iwa"
            || name.starts_with("Index/Slide-"))
    {
        return Ok(Some("application/vnd.apple.keynote.13".to_owned()));
    }
    // IPADetector.
    let left = ctx.ipa_left.get_or_insert([true; 6]);
    for (k, p) in IPA_PATTERNS.iter().enumerate() {
        if p(name)
            && let Some(l) = left.get_mut(k)
        {
            *l = false;
        }
    }
    if left.iter().all(|l| !l) {
        return Ok(Some("application/x-itunes-ipa".to_owned()));
    }
    // JarDetector.
    if name == "AndroidManifest.xml" {
        return Ok(Some("application/vnd.android.package-archive".to_owned()));
    }
    if name == "META-INF/MANIFEST.MF" {
        ctx.jar_manifest = true;
    }
    if ctx.jar_manifest {
        if name == "WEB-INF/" {
            return Ok(Some("application/x-tika-java-web-archive".to_owned()));
        }
        if name == "META-INF/application.xml" {
            return Ok(Some(
                "application/x-tika-java-enterprise-archive".to_owned(),
            ));
        }
    }
    // KMZDetector.
    if !name.contains(['/', '\\']) && name.ends_with(".kml") {
        *ctx.kml.get_or_insert(0) += 1;
    }
    // OpenDocumentDetector.
    if name == "mimetype" {
        let (data, err) = w.content(e, MAX_MIMETYPE);
        if let Some(err) = err {
            return Err(err);
        }
        if !data.is_empty()
            && let Some(t) = media::parse(&String::from_utf8_lossy(&data))
        {
            return Ok(Some(t));
        }
    }
    // StarOfficeDetector.
    if name == "META-INF/manifest.xml" {
        let (data, err) = w.content(e, MAX_MANIFEST);
        if let Some(err) = err {
            return Err(err);
        }
        if let Some(t) = star_office(&data) {
            return Ok(Some(t));
        }
    }
    // FrictionlessPackageDetector.
    let parts = ctx.parts.get_or_insert([false; 4]);
    if name.starts_with("archive/") {
        parts[1] = true;
    } else if name.starts_with("indexes/") {
        parts[2] = true;
    } else if name.starts_with("pages/") {
        parts[3] = true;
    } else if name == "datapackage.json" {
        parts[0] = true;
    }
    if frictionless(Some(*parts)) == Some("application/x-wacz") {
        return Ok(Some("application/x-wacz".to_owned()));
    }
    // OPCPackageDetector.
    if name == "[Content_Types].xml" {
        let (data, _) = w.content(e, MAX_XML_PART);
        return Ok(Some(
            content_types(&data)
                .unwrap_or("application/x-tika-ooxml")
                .to_owned(),
        ));
    }
    if OOXML_HINTS.contains(&name) {
        *ctx.ooxml_hints.get_or_insert(0) += 1;
    }
    Ok(None)
}

fn frictionless(parts: Option<[bool; 4]>) -> Option<&'static str> {
    let p = parts?;
    if !p[0] {
        return None;
    }
    let n = p.iter().filter(|x| **x).count();
    if n == 1 {
        return Some("application/x-vnd.datapackage+zip");
    }
    if p[1] || (p[2] && p[3]) {
        return Some("application/x-wacz");
    }
    None
}

/// Every detector's `streamingDetectFinal`.
fn finish(ctx: &Ctx) -> String {
    if ctx.seen_any {
        if ctx.iwork13_main {
            return "application/vnd.apple.unknown.13".into();
        }
        if ctx.iwork_common {
            return "application/vnd.apple.iwork".into();
        }
    }
    if ctx.jar_manifest {
        return "application/java-archive".into();
    }
    if ctx.kml == Some(1) {
        return "application/vnd.google-earth.kmz".into();
    }
    if let Some(t) = frictionless(ctx.parts) {
        return t.into();
    }
    if ctx.ooxml_hints.is_some_and(|n| n > 2) {
        return "application/x-tika-ooxml".into();
    }
    media::APPLICATION_ZIP.into()
}

/// `detectStreaming`: walks the entries; a data-descriptor problem
/// restarts the walk with stored entries allowed (fresh context).
fn detect_streaming(src: &dyn ReadAt, allow_stored_dd: bool) -> String {
    let mut w = Walker::new(src, allow_stored_dd);
    let mut ctx = Ctx::default();
    loop {
        let e = match w.next_entry() {
            Ok(Some(e)) => e,
            Err(ZipErr::DataDescriptor) if !allow_stored_dd => return detect_streaming(src, true),
            Ok(None) | Err(_) => break,
        };
        match update(&mut w, &e, &mut ctx) {
            Ok(Some(t)) => return t,
            Ok(None) => {}
            Err(ZipErr::DataDescriptor) if !allow_stored_dd => return detect_streaming(src, true),
            Err(_) => break,
        }
    }
    finish(&ctx)
}

/// TIFF signatures checked before the archive sniffing.
fn is_tiff(prefix: &[u8; 1024]) -> bool {
    prefix.starts_with(b"MM\x00\x2a")
        || prefix.starts_with(b"II\x2a\x00")
        || prefix.starts_with(b"MM\x00\x2b")
}

/// `DefaultZipContainerDetector.detect`: `None` for octet-stream.
pub(crate) fn detect(src: &dyn ReadAt) -> Option<String> {
    let mut prefix = [0u8; 1024];
    let n = src.read_at(0, &mut prefix).unwrap_or(0);
    if is_tiff(&prefix) {
        return Some("image/tiff".into());
    }
    let head = prefix.get(..n).unwrap_or_default();
    match archive::archive(head) {
        Some(Archive::Zip) => Some(detect_streaming(src, false)),
        Some(a) => Some(archive::archive_media_type(a).to_owned()),
        None => archive::compressor(head).map(str::to_owned),
    }
}

/// Whether any local file header of a ZIP (within its first 16 MiB) has
/// the encryption flag set.
pub fn has_encrypted_entry(src: &dyn ReadAt) -> bool {
    let mut w = Walker::new(src, true);
    while let Ok(Some(e)) = w.next_entry() {
        if e.encrypted() {
            return true;
        }
    }
    false
}

/// Builds a ZIP file for tests: `(name, data, stored)`.
#[cfg(any(test, feature = "builder"))]
pub fn build_zip(entries: &[(&str, &[u8])], encrypt: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let off = out.len() as u32;
        let flags = u16::from(encrypt.contains(name));
        let crc = crc32(data);
        let mut h = Vec::new();
        h.extend_from_slice(LFH);
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(&flags.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&[0; 4]);
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        let mut c = Vec::new();
        c.extend_from_slice(CFH);
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&flags.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&[0; 4]);
        c.extend_from_slice(&crc.to_le_bytes());
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(&(name.len() as u16).to_le_bytes());
        c.extend_from_slice(&[0; 12]);
        c.extend_from_slice(&off.to_le_bytes());
        c.extend_from_slice(name.as_bytes());
        central.extend(c);
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(EOCD);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out
}

#[cfg(any(test, feature = "builder"))]
fn crc32(d: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in d {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    fn det(entries: &[(&str, &[u8])]) -> Option<String> {
        detect(&MemSource::new(build_zip(entries, &[])))
    }

    #[test]
    fn containers() {
        assert_eq!(det(&[("a.txt", b"x")]).as_deref(), Some("application/zip"));
        assert_eq!(
            det(&[
                ("mimetype", b"application/vnd.oasis.opendocument.text"),
                ("content.xml", b"<a/>")
            ])
            .as_deref(),
            Some("application/vnd.oasis.opendocument.text")
        );
        assert_eq!(
            det(&[
                ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
                ("a.class", b"\xCA\xFE")
            ])
            .as_deref(),
            Some("application/java-archive")
        );
        assert_eq!(
            det(&[
                ("META-INF/MANIFEST.MF", b"m"),
                ("AndroidManifest.xml", b"x")
            ])
            .as_deref(),
            Some("application/vnd.android.package-archive")
        );
        assert_eq!(
            det(&[("doc.kml", b"<kml/>")]).as_deref(),
            Some("application/vnd.google-earth.kmz")
        );
        assert_eq!(
            det(&[("a.kml", b"1"), ("b.kml", b"2")]).as_deref(),
            Some("application/zip")
        );
        let ct = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        assert_eq!(
            det(&[("[Content_Types].xml", ct)]).as_deref(),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        );
        assert_eq!(
            det(&[("[Content_Types].xml", b"junk")]).as_deref(),
            Some("application/x-tika-ooxml")
        );
        assert_eq!(
            det(&[
                ("word/document.xml", b""),
                ("_rels/.rels", b""),
                ("xl/workbook.xml", b"")
            ])
            .as_deref(),
            Some("application/x-tika-ooxml")
        );
        assert_eq!(
            det(&[("datapackage.json", b"{}")]).as_deref(),
            Some("application/x-vnd.datapackage+zip")
        );
        assert_eq!(
            det(&[("datapackage.json", b"{}"), ("archive/x.warc", b"")]).as_deref(),
            Some("application/x-wacz")
        );
        let manifest = br#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:media-type="application/vnd.sun.xml.calc" manifest:full-path="/"/></manifest:manifest>"#;
        assert_eq!(
            det(&[("META-INF/manifest.xml", manifest)]).as_deref(),
            Some("application/vnd.sun.xml.calc")
        );
        let ipa: Vec<(&str, &[u8])> = vec![
            ("Payload/", b""),
            ("Payload/X.app/", b""),
            ("Payload/X.app/_CodeSignature/", b""),
            ("Payload/X.app/_CodeSignature/CodeResources", b""),
            ("Payload/X.app/Info.plist", b""),
            ("Payload/X.app/PkgInfo", b""),
        ];
        assert_eq!(det(&ipa).as_deref(), Some("application/x-itunes-ipa"));
        assert_eq!(
            det(&[("Index/Document.iwa", b"")]).as_deref(),
            Some("application/vnd.apple.unknown.13")
        );
    }

    #[test]
    fn deflated_and_encrypted() {
        // An encrypted mimetype makes the ODF detector throw: plain zip.
        let z = build_zip(
            &[("mimetype", b"application/vnd.oasis.opendocument.text")],
            &["mimetype"],
        );
        assert_eq!(
            detect(&MemSource::new(z.clone())).as_deref(),
            Some("application/zip")
        );
        assert!(has_encrypted_entry(&MemSource::new(z)));
        assert!(!has_encrypted_entry(&MemSource::new(build_zip(
            &[("a", b"b")],
            &[]
        ))));
    }

    #[test]
    fn non_zip() {
        assert_eq!(
            detect(&MemSource::new(b"MM\x00\x2a....".to_vec())).as_deref(),
            Some("image/tiff")
        );
        assert_eq!(
            detect(&MemSource::new(b"BZh91AY&SY".to_vec())).as_deref(),
            Some("application/x-bzip2")
        );
        assert_eq!(detect(&MemSource::new(b"hello".to_vec())), None);
        // Truncated zip: whatever was seen.
        let mut z = build_zip(&[("META-INF/MANIFEST.MF", b"m"), ("b", b"c")], &[]);
        z.truncate(60);
        assert_eq!(
            detect(&MemSource::new(z)).as_deref(),
            Some("application/java-archive")
        );
    }
}

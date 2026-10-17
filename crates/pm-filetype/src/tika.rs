//! `new Tika().detect(stream[, name])`: Tika's `DefaultDetector`, a
//! composite of the container detectors (in tika-app's order: Ogg first,
//! then by class name) and the `MimeTypes` database last. A detector's
//! answer replaces the current one only if it is a specialisation of it.

use std::io::Read;

use pm_core::ReadAt;

use crate::db::{MAGIC_PREFIX_LEN, MimeDb};
use crate::{Failure, bplist, media, ogg, ole, zip};

/// The results of the container detectors, which do not depend on the
/// file name.
#[derive(Debug, Clone, Default)]
pub(crate) struct Containers {
    found: Vec<String>,
}

/// `GZipSpecializationDetector`.
fn gzip(prefix: &[u8]) -> Option<&'static str> {
    if !prefix.starts_with(&[0x1F, 0x8B]) {
        return None;
    }
    let head = prefix.get(..prefix.len().min(1024)).unwrap_or(prefix);
    let mut dec = flate2::read::GzDecoder::new(head);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match dec.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(buf.get(..n).unwrap_or_default()),
        }
        if out.len() > 1 << 20 {
            break;
        }
    }
    Some(if out.starts_with(b"WARC/") {
        "application/warc+gz"
    } else if out.starts_with(b"filedesc://") {
        "application/arc+gz"
    } else {
        "application/gzip"
    })
}

impl Containers {
    /// Runs the container detectors over `src` (`prefix`: its first
    /// bytes, at least 1 KiB when available).
    pub(crate) fn run(src: &dyn ReadAt, prefix: &[u8]) -> Result<Self, Failure> {
        let mut found = Vec::new();
        if prefix.starts_with(b"OggS")
            && let Some(t) = ogg::detect(src)?
        {
            found.push(t.to_owned());
        }
        if prefix.starts_with(b"bplist")
            && let Some(t) = bplist::detect(src)?
        {
            found.push(t.to_owned());
        }
        if let Some(t) = gzip(prefix) {
            found.push(t.to_owned());
        }
        if let Some((p, m)) = ole::detect(src, prefix) {
            found.push(p);
            found.push(m);
        }
        if let Some(t) = zip::detect(src) {
            found.push(t);
        }
        Ok(Self { found })
    }
}

/// `MimeTypes.detect` on the magic prefix with an optional file name.
pub(crate) fn mime_types(db: &MimeDb, prefix: &[u8], name: Option<&str>) -> String {
    let mut possible = db.magic_types(prefix);
    if let Some(name) = name.and_then(resource_name) {
        let hint = db.type_for_name(&name);
        possible = db.apply_hint(possible, hint);
    }
    possible.first().map_or_else(
        || media::OCTET_STREAM.to_owned(),
        |&t| db.name(t).to_owned(),
    )
}

/// The composite result.
pub(crate) fn combine(db: &MimeDb, containers: &Containers, magic: &str) -> String {
    let reg = db.registry();
    let mut t = media::OCTET_STREAM.to_owned();
    for d in containers
        .found
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(magic))
    {
        if reg.is_specialization_of(d, &t) {
            d.clone_into(&mut t);
        }
    }
    t
}

/// Tika's detection of `src` (optionally with its file name).
pub fn detect(src: &dyn ReadAt, name: Option<&str>) -> Result<String, String> {
    let db = MimeDb::get();
    let prefix = read_prefix(src);
    let containers = Containers::run(src, &prefix).map_err(|f| f.0)?;
    let magic = mime_types(db, &prefix, name);
    Ok(combine(db, &containers, &magic))
}

/// The first 64 KiB.
pub(crate) fn read_prefix(src: &dyn ReadAt) -> Vec<u8> {
    let n = usize::try_from(src.size().min(MAGIC_PREFIX_LEN as u64)).unwrap_or(0);
    let mut buf = vec![0u8; n];
    let got = src.read_at(0, &mut buf).unwrap_or(0);
    buf.truncate(got);
    buf
}

fn is_hex(c: char) -> bool {
    c.is_ascii_hexdigit()
}

/// The file-name part `MimeTypes.detect` takes from the resource name,
/// parsed as a `java.net.URI`: `None` when no name hint applies (an opaque
/// URI such as `file.txt:stream`).
pub(crate) fn resource_name(name: &str) -> Option<String> {
    let Some(path) = uri_path(name) else {
        // URISyntaxException: the whole string is the name.
        return Some(name.to_owned());
    };
    let path = path?;
    let slash = path.rfind('/').map_or(0, |i| i + 1);
    let n = path.get(slash..).unwrap_or_default();
    (!n.is_empty()).then(|| n.to_owned())
}

/// `Some(Some(path))`: a hierarchical URI's decoded path; `Some(None)`:
/// a URI without a path (opaque); `None`: not a valid URI.
#[allow(clippy::option_option, reason = "mirrors URI parse outcomes")]
fn uri_path(s: &str) -> Option<Option<String>> {
    let illegal = |c: char| {
        c.is_control()
            || c == ' '
            || "\"<>\\^`{|}".contains(c)
            || (c as u32 > 0x7F && c.is_whitespace())
    };
    // Fragment and query.
    let (before_frag, frag) = match s.find('#') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    if before_frag
        .chars()
        .any(|c| illegal(c) || c == '[' || c == ']')
        || frag.is_some_and(|f| {
            f.chars()
                .any(|c| illegal(c) || c == '#' || c == '[' || c == ']')
        })
    {
        return None;
    }
    // Percent escapes must be complete.
    let bytes: Vec<char> = s.chars().collect();
    for (i, &c) in bytes.iter().enumerate() {
        if c == '%'
            && !(bytes.get(i + 1).is_some_and(|c| is_hex(*c))
                && bytes.get(i + 2).is_some_and(|c| is_hex(*c)))
        {
            return None;
        }
    }
    let colon = before_frag.find(':');
    let first_delim = before_frag.find(['/', '?']);
    let rest = match colon {
        Some(c) if first_delim.is_none_or(|d| c < d) => {
            let scheme = &before_frag[..c];
            let mut ch = scheme.chars();
            let ok = ch.next().is_some_and(|c| c.is_ascii_alphabetic())
                && ch.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
            if !ok {
                return None;
            }
            let ssp = &before_frag[c + 1..];
            if ssp.is_empty() {
                return None;
            }
            if !ssp.starts_with('/') {
                return Some(None);
            }
            ssp
        }
        _ => before_frag,
    };
    // Authority.
    let rest = if let Some(a) = rest.strip_prefix("//") {
        match a.find(['/', '?']) {
            Some(i) => &a[i..],
            None => "",
        }
    } else {
        rest
    };
    let path = match rest.find('?') {
        Some(i) => &rest[..i],
        None => rest,
    };
    Some(Some(percent_decode(path)))
}

fn percent_decode(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b.get(i).copied().unwrap_or(0);
        if c == b'%' {
            let h = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok());
            if let Some(v) = h.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    fn t(data: &[u8], name: Option<&str>) -> String {
        detect(&MemSource::new(data.to_vec()), name).unwrap()
    }

    #[test]
    fn names() {
        assert_eq!(resource_name("report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(resource_name("my file.txt").as_deref(), Some("my file.txt"));
        assert_eq!(resource_name("a.txt:Zone.Identifier"), None);
        assert_eq!(resource_name("my_file:x").as_deref(), Some("my_file:x"));
        assert_eq!(resource_name("a%20b.doc").as_deref(), Some("a b.doc"));
        assert_eq!(resource_name("x.doc#frag").as_deref(), Some("x.doc"));
        assert_eq!(resource_name("100%.txt").as_deref(), Some("100%.txt"));
        assert_eq!(resource_name("q?x.doc").as_deref(), Some("q"));
    }

    #[test]
    fn common_types() {
        assert_eq!(t(b"%PDF-1.4\n%....", None), "application/pdf");
        assert_eq!(t(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR", None), "image/png");
        assert_eq!(t(b"hello world, plain text", None), "text/plain");
        assert_eq!(
            t(b"hello world, plain text", Some("x.java")),
            "text/x-java-source"
        );
        assert_eq!(
            t(&[0u8, 1, 2, 3, 0xFF, 0xFE, 7, 8], None),
            "application/octet-stream"
        );
        assert_eq!(
            t(
                b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
                None
            ),
            "image/svg+xml"
        );
        assert_eq!(t(b"<?xml version=\"1.0\"?>junk", None), "text/plain");
        assert_eq!(t(b"<html><body>hi</body></html>", None), "text/html");
    }
}

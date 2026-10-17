//! XML: a tolerant `quick-xml` event walker shared by the XML, Office and
//! EPUB extractors, and the generic XML extractor (text and CDATA
//! content, one line per element).

use std::io::BufRead;

use encoding_rs::{Encoding, UTF_8};
use pm_core::ReadAt;
use quick_xml::events::{BytesRef, Event};

use crate::util::SrcReader;
use crate::{Metadata, Stop, TextError, TextSink};

/// One XML event, reduced to what the extractors need.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Ev<'a> {
    /// Start tag (also emitted for empty elements, followed by `End`):
    /// local name and the raw start tag for attribute lookups.
    Start(&'a [u8], &'a quick_xml::events::BytesStart<'a>),
    End(&'a [u8]),
    /// Character data (text, CDATA, resolved references).
    Text(&'a str),
}

/// Walks an XML document, calling `f` for each event. Errors in the
/// document end the walk (text already seen stays); `Stop` from `f` is
/// returned.
pub(crate) fn walk<R: BufRead>(
    reader: R,
    mut f: impl FnMut(Ev<'_>) -> Result<(), Stop>,
) -> Result<(), Stop> {
    let mut r = quick_xml::Reader::from_reader(reader);
    {
        let c = r.config_mut();
        c.check_end_names = false;
        c.allow_unmatched_ends = true;
        c.allow_dangling_amp = true;
        c.check_comments = false;
    }
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let ev = match r.read_event_into(&mut buf) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("XML error: {e}");
                return Ok(());
            }
        };
        match ev {
            Event::Start(s) => {
                let name = s.local_name();
                f(Ev::Start(name.as_ref(), &s))?;
            }
            Event::Empty(s) => {
                let name = s.local_name();
                f(Ev::Start(name.as_ref(), &s))?;
                f(Ev::End(name.as_ref()))?;
            }
            Event::End(e) => {
                let name = e.local_name();
                f(Ev::End(name.as_ref()))?;
            }
            Event::Text(t) => {
                if let Ok(s) = t.decode() {
                    f(Ev::Text(&s))?;
                }
            }
            Event::CData(t) => {
                if let Ok(s) = t.decode() {
                    f(Ev::Text(&s))?;
                }
            }
            Event::GeneralRef(r) => {
                if let Some(c) = resolve_ref(&r) {
                    let mut b = [0u8; 4];
                    f(Ev::Text(c.encode_utf8(&mut b)))?;
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

fn resolve_ref(r: &BytesRef<'_>) -> Option<char> {
    if r.is_char_ref() {
        return r.resolve_char_ref().ok().flatten();
    }
    let name = r.decode().ok()?;
    Some(match name.as_ref() {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{A0}',
        _ => return None,
    })
}

/// Value of the attribute with local name `name`.
pub(crate) fn attr(s: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<String> {
    for a in s.attributes().with_checks(false).flatten() {
        if a.key.local_name().as_ref() == name {
            return a
                .decoded_and_normalized_value(quick_xml::XmlVersion::default(), s.decoder())
                .ok()
                .map(std::borrow::Cow::into_owned);
        }
    }
    None
}

/// Generic XML: text content, a newline after each element that had text.
pub(crate) fn extract(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<Metadata, TextError> {
    let enc = detect(src)?;
    let mut out = String::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut String| {
        let t = cur.trim();
        if !t.is_empty() {
            out.push_str(t);
            out.push('\n');
        }
        cur.clear();
    };
    let mut emit = |ev: Ev<'_>| -> Result<(), Stop> {
        match ev {
            Ev::Text(t) => {
                if cur.len() < 16 * 1024 * 1024 {
                    cur.push_str(t);
                }
            }
            Ev::Start(..) | Ev::End(_) => flush(&mut cur, &mut out),
        }
        if out.len() >= 64 * 1024 {
            sink.push(&out)?;
            out.clear();
        }
        Ok(())
    };
    if enc == UTF_8 {
        walk(SrcReader::new(src), &mut emit)?;
    } else {
        walk(DecodingReader::new(src, enc), &mut emit)?;
    }
    flush(&mut cur, &mut out);
    if !out.is_empty() {
        sink.push(&out)?;
    }
    Ok(Metadata::new())
}

/// The document encoding: BOM, else the XML declaration, else UTF-8.
fn detect(src: &dyn ReadAt) -> Result<&'static Encoding, TextError> {
    let mut head = vec![0u8; usize::try_from(src.size().min(512)).unwrap_or(0)];
    let n = src.read_at(0, &mut head)?;
    head.truncate(n);
    if let Some((enc, _)) = Encoding::for_bom(&head) {
        return Ok(enc);
    }
    // UTF-16 without BOM: "<\0?\0" / "\0<\0?".
    if head.starts_with(b"<\0?\0") {
        return Ok(encoding_rs::UTF_16LE);
    }
    if head.starts_with(b"\0<\0?") {
        return Ok(encoding_rs::UTF_16BE);
    }
    if let Some(p) = head.windows(9).position(|w| w == b"encoding=") {
        let rest = head.get(p + 10..).unwrap_or_default();
        let label: Vec<u8> = rest
            .iter()
            .copied()
            .take_while(|&c| c != b'"' && c != b'\'')
            .take(40)
            .collect();
        if let Some(enc) = Encoding::for_label(&label)
            && enc != encoding_rs::UTF_16LE
            && enc != encoding_rs::UTF_16BE
        {
            return Ok(enc);
        }
    }
    Ok(UTF_8)
}

/// Decodes a source to UTF-8 on the fly (for non-UTF-8 XML).
pub(crate) struct DecodingReader<'a> {
    inner: SrcReader<'a>,
    dec: encoding_rs::Decoder,
    out: Vec<u8>,
    pos: usize,
    done: bool,
}

impl<'a> DecodingReader<'a> {
    pub(crate) fn new(src: &'a dyn ReadAt, enc: &'static Encoding) -> Self {
        Self {
            inner: SrcReader::new(src),
            dec: enc.new_decoder_with_bom_removal(),
            out: Vec::new(),
            pos: 0,
            done: false,
        }
    }
}

impl std::io::Read for DecodingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let avail = self.fill_buf()?;
        let n = avail.len().min(buf.len());
        if let (Some(d), Some(s)) = (buf.get_mut(..n), avail.get(..n)) {
            d.copy_from_slice(s);
        }
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for DecodingReader<'_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        while self.pos >= self.out.len() && !self.done {
            let input = self.inner.fill_buf()?.to_vec();
            let last = input.is_empty();
            self.inner.consume(input.len());
            let mut s = String::with_capacity(input.len() * 2 + 16);
            let _ = self.dec.decode_to_string(&input, &mut s, last);
            self.out = s.into_bytes();
            self.pos = 0;
            self.done = last;
        }
        Ok(self.out.get(self.pos..).unwrap_or_default())
    }

    fn consume(&mut self, amt: usize) {
        self.pos = (self.pos + amt).min(self.out.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringSink;

    fn run(xml: &[u8]) -> String {
        let src = pm_core::io::MemSource::new(xml.to_vec());
        let mut sink = StringSink::default();
        extract(&src, &mut sink).unwrap();
        sink.text
    }

    #[test]
    fn text_and_cdata() {
        assert_eq!(
            run(b"<?xml version=\"1.0\"?><a><b>one &amp; two</b><c><![CDATA[<raw>]]></c><d/>x&#65;</a>"),
            "one & two\n<raw>\nxA\n"
        );
    }

    #[test]
    fn utf16_and_declared_encodings() {
        let mut x = vec![0xFF, 0xFE];
        for u in "<a>Grüße</a>".encode_utf16() {
            x.extend(u.to_le_bytes());
        }
        assert_eq!(run(&x), "Grüße\n");
        assert_eq!(
            run(b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a>K\xF6ln</a>"),
            "Köln\n"
        );
    }

    #[test]
    fn malformed_keeps_text_so_far() {
        assert_eq!(run(b"<a>ok</a><b>also</c><"), "ok\nalso\n");
    }
}

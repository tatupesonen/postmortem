//! HTML and the other web types (Autopsy `HtmlTextExtractor`, which uses
//! Jericho's `Renderer`).
//!
//! A tolerant streaming tokenizer renders text the way Jericho does in
//! spirit: block elements and `<br>` become line breaks, table cells are
//! separated by tabs, whitespace collapses outside `<pre>`, entities are
//! decoded, and script/style contents are not text. Metadata is Autopsy's:
//! numbered lists of `Scripts`, `Links`, `Images`, `Comments` and every
//! `Others` tag that has attributes.

use std::fmt::Write as _;

use encoding_rs::Encoding;
use pm_core::ReadAt;

use crate::{Metadata, Stop, TextError, TextSink, encoding};

const BLOCK: usize = 64 * 1024;
/// Output is pushed in pieces of about this size.
const FLUSH: usize = 64 * 1024;
/// Cap on each metadata list (bytes).
const MAX_META: usize = 1024 * 1024;
/// An unterminated construct longer than this is treated as text.
const MAX_PENDING: usize = 256 * 1024;

pub(crate) fn extract(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<Metadata, TextError> {
    let enc = detect(src)?;
    let mut r = Renderer::new(sink);
    let mut dec = enc.new_decoder_with_bom_removal();
    let size = src.size();
    let mut raw = vec![0u8; BLOCK];
    let mut pending = String::new();
    let mut off = 0u64;
    loop {
        let want = usize::try_from((size - off.min(size)).min(BLOCK as u64)).unwrap_or(0);
        let n = if want == 0 {
            0
        } else {
            src.read_at(off, raw.get_mut(..want).unwrap_or_default())?
        };
        off += n as u64;
        let last = n == 0;
        let input = raw.get(..n).unwrap_or_default();
        pending.reserve(
            dec.max_utf8_buffer_length(input.len())
                .unwrap_or(input.len() * 3),
        );
        let _ = dec.decode_to_string(input, &mut pending, last);
        let used = r.feed(&pending, last)?;
        pending.drain(..used);
        if last {
            r.finish()?;
            return Ok(r.metadata());
        }
    }
}

/// Renders a complete HTML string (used for HTML e-mail bodies).
pub(crate) fn render_str(html: &str, sink: &mut dyn TextSink) -> Result<Metadata, Stop> {
    let mut r = Renderer::new(sink);
    r.feed(html, true)?;
    r.finish()?;
    Ok(r.metadata())
}

/// BOM, then a `charset` in the first 1024 bytes, then content detection.
fn detect(src: &dyn ReadAt) -> Result<&'static Encoding, TextError> {
    let mut head =
        vec![0u8; usize::try_from(src.size().min(encoding::DETECT_BYTES as u64)).unwrap_or(0)];
    let n = src.read_at(0, &mut head)?;
    head.truncate(n);
    if let Some((enc, _)) = Encoding::for_bom(&head) {
        return Ok(enc);
    }
    if let Some(enc) = meta_charset(head.get(..head.len().min(1024)).unwrap_or_default()) {
        return Ok(enc);
    }
    Ok(encoding::detect(&head))
}

fn meta_charset(head: &[u8]) -> Option<&'static Encoding> {
    let lower: Vec<u8> = head.iter().map(u8::to_ascii_lowercase).collect();
    let pos = lower.windows(8).position(|w| w == b"charset=")?;
    let rest = lower.get(pos + 8..)?;
    let rest: Vec<u8> = rest
        .iter()
        .copied()
        .skip_while(|&c| c == b'"' || c == b'\'' || c == b' ')
        .take_while(|&c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b':'))
        .collect();
    let enc = Encoding::for_label(&rest)?;
    // A page that claims UTF-16 in an ASCII-compatible meta tag is not.
    if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
        return Some(encoding_rs::UTF_8);
    }
    Some(enc)
}

/// Elements whose start and end break the line.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "tr"
            | "blockquote"
            | "pre"
            | "address"
            | "center"
            | "form"
            | "fieldset"
            | "hr"
            | "section"
            | "article"
            | "aside"
            | "header"
            | "footer"
            | "nav"
            | "main"
            | "figure"
            | "figcaption"
            | "title"
            | "caption"
            | "tbody"
            | "thead"
            | "tfoot"
            | "option"
            | "body"
            | "html"
            | "head"
    )
}

/// Elements whose content is raw text (not markup).
fn raw_text_element(name: &str) -> Option<&'static str> {
    match name {
        "script" => Some("script"),
        "style" => Some("style"),
        "textarea" => Some("textarea"),
        "xmp" => Some("xmp"),
        _ => None,
    }
}

struct Renderer<'a> {
    sink: &'a mut dyn TextSink,
    out: String,
    /// Inside `<pre>` (depth).
    pre: u32,
    /// Raw-text element being read (`script`, `style`, …) and its
    /// collected content (scripts only).
    raw: Option<&'static str>,
    raw_content: String,
    /// Pending script tag content for the metadata entry.
    script_tag: String,
    /// The last character written (for whitespace collapsing).
    last: char,
    /// A table cell has been opened on this row.
    cell_open: bool,
    scripts: (usize, String),
    links: (usize, String),
    images: (usize, String),
    comments: (usize, String),
    others: (usize, String),
}

impl<'a> Renderer<'a> {
    fn new(sink: &'a mut dyn TextSink) -> Self {
        Self {
            sink,
            out: String::new(),
            pre: 0,
            raw: None,
            raw_content: String::new(),
            script_tag: String::new(),
            last: '\n',
            cell_open: false,
            scripts: (0, "\n".into()),
            links: (0, "\n".into()),
            images: (0, "\n".into()),
            comments: (0, "\n".into()),
            others: (0, "\n".into()),
        }
    }

    fn metadata(&self) -> Metadata {
        let mut m = Metadata::new();
        for (key, (n, text)) in [
            ("Scripts", &self.scripts),
            ("Links", &self.links),
            ("Images", &self.images),
            ("Comments", &self.comments),
            ("Others", &self.others),
        ] {
            if *n > 0 {
                m.insert(key.into(), text.clone());
            }
        }
        m
    }

    fn flush(&mut self) -> Result<(), Stop> {
        if !self.out.is_empty() {
            self.sink.push(&self.out)?;
            self.out.clear();
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Stop> {
        if let Some(name) = self.raw.take() {
            let content = std::mem::take(&mut self.raw_content);
            self.end_raw(name, &content);
        }
        self.flush()
    }

    fn put(&mut self, c: char) {
        self.out.push(c);
        self.last = c;
    }

    fn newline(&mut self) {
        if self.last != '\n' {
            // Drop a trailing collapsed space before the break.
            if self.out.ends_with(' ') {
                self.out.pop();
            }
            self.put('\n');
        }
    }

    fn text(&mut self, s: &str) {
        let decoded = decode_entities(s);
        for c in decoded.chars() {
            if self.pre > 0 {
                self.put(c);
            } else if c.is_whitespace() && c != '\u{A0}' {
                if !self.last.is_whitespace() {
                    self.put(' ');
                }
            } else if c == '\u{A0}' {
                self.put(' ');
            } else {
                self.put(c);
            }
        }
    }

    /// Consumes as much of `buf` as possible; returns the bytes used (the
    /// rest is an unfinished construct to retry with more input).
    fn feed(&mut self, buf: &str, last: bool) -> Result<usize, Stop> {
        let bytes = buf.as_bytes();
        let mut i = 0usize;
        while i < buf.len() {
            if self.out.len() >= FLUSH {
                self.flush()?;
            }
            if let Some(name) = self.raw {
                let rest = buf.get(i..).unwrap_or_default();
                match find_ci(rest, &format!("</{name}")) {
                    Some(j) => {
                        self.raw_content.push_str(rest.get(..j).unwrap_or_default());
                        let content = std::mem::take(&mut self.raw_content);
                        self.raw = None;
                        self.end_raw(name, &content);
                        // Skip the end tag.
                        let after = i + j;
                        match buf.get(after..).and_then(|r| r.find('>')) {
                            Some(k) => i = after + k + 1,
                            None if last => return Ok(buf.len()),
                            None => return Ok(after.max(i)),
                        }
                    }
                    None if last => {
                        self.raw_content.push_str(rest);
                        return Ok(buf.len());
                    }
                    None => {
                        // Keep a tail that could hold a partial end tag.
                        let keep = name.len() + 2;
                        let mut cut = buf.len().saturating_sub(keep).max(i);
                        while !buf.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        if self.raw_content.len() < MAX_META {
                            self.raw_content
                                .push_str(buf.get(i..cut).unwrap_or_default());
                        }
                        return Ok(cut);
                    }
                }
                continue;
            }
            let rest = buf.get(i..).unwrap_or_default();
            let Some(lt) = rest.find('<') else {
                // Text up to the end; hold back a possibly split entity.
                let mut cut = rest.len();
                if !last
                    && let Some(amp) = rest.rfind('&')
                    && rest.len() - amp < 40
                    && !rest.get(amp..).unwrap_or_default().contains(';')
                {
                    cut = amp;
                }
                self.text(rest.get(..cut).unwrap_or_default());
                return Ok(i + cut);
            };
            if lt > 0 {
                self.text(rest.get(..lt).unwrap_or_default());
            }
            let at = i + lt;
            let tail = buf.get(at..).unwrap_or_default();
            if let Some(used) = self.markup(tail, last) {
                i = at + used;
            } else {
                if !last && tail.len() < MAX_PENDING {
                    return Ok(at);
                }
                // Unterminated: the '<' is text.
                self.text("<");
                i = at + 1;
            }
            let _ = bytes;
        }
        Ok(buf.len())
    }

    /// Handles markup at the start of `s` (which begins with '<'); returns
    /// the bytes used, or `None` if it is incomplete.
    fn markup(&mut self, s: &str, last: bool) -> Option<usize> {
        if let Some(body) = s.strip_prefix("<!--") {
            let end = body.find("-->")?;
            let content = body.get(..end).unwrap_or_default();
            add(&mut self.comments, content);
            return Some(4 + end + 3);
        }
        if s.starts_with("<!") || s.starts_with("<?") {
            let end = s.find('>')?;
            return Some(end + 1);
        }
        if let Some(body) = s.strip_prefix("</") {
            let first = body.chars().next();
            if !first.is_some_and(|c| c.is_ascii_alphabetic()) {
                if body.is_empty() && !last {
                    return None;
                }
                self.text("</");
                return Some(2);
            }
            let end = body.find('>')?;
            let name = tag_name(body);
            self.end_tag(&name);
            return Some(2 + end + 1);
        }
        let body = s.get(1..).unwrap_or_default();
        match body.chars().next() {
            Some(c) if c.is_ascii_alphabetic() => {}
            None if !last => return None,
            _ => {
                self.text("<");
                return Some(1);
            }
        }
        let end = tag_end(body)?;
        let inner = body.get(..end).unwrap_or_default();
        let name = tag_name(inner);
        let content = inner.get(name.len()..).unwrap_or_default();
        let content = content.strip_suffix('/').unwrap_or(content);
        self.start_tag(&name, content);
        Some(1 + end + 1)
    }

    fn start_tag(&mut self, name: &str, content: &str) {
        match name {
            "script" => {
                self.scripts.0 += 1;
                content.clone_into(&mut self.script_tag);
            }
            "a" => add(&mut self.links, content),
            "img" => add(&mut self.images, content),
            _ => {
                if has_attributes(content) {
                    self.others.0 += 1;
                    if self.others.1.len() < MAX_META {
                        let n = self.others.0;
                        let _ = writeln!(self.others.1, "{n}) {name}:{content}");
                    }
                }
            }
        }
        if let Some(raw) = raw_text_element(name) {
            self.raw = Some(raw);
            self.raw_content.clear();
            return;
        }
        match name {
            "br" => self.put('\n'),
            "td" | "th" => {
                if self.cell_open {
                    if self.out.ends_with(' ') {
                        self.out.pop();
                    }
                    self.put('\t');
                }
                self.cell_open = true;
            }
            "tr" => {
                self.cell_open = false;
                self.newline();
            }
            "pre" => {
                self.newline();
                self.pre += 1;
            }
            "li" => {
                self.newline();
                self.out.push_str("* ");
                self.last = ' ';
            }
            _ if is_block(name) => self.newline(),
            _ => {}
        }
    }

    fn end_tag(&mut self, name: &str) {
        match name {
            "pre" => {
                self.pre = self.pre.saturating_sub(1);
                self.newline();
            }
            "tr" => {
                self.cell_open = false;
                self.newline();
            }
            "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" | "blockquote" => {
                self.newline();
                self.put('\n');
            }
            _ if is_block(name) => self.newline(),
            _ => {}
        }
    }

    fn end_raw(&mut self, name: &str, content: &str) {
        if name == "script" {
            let n = self.scripts.0;
            if self.scripts.1.len() < MAX_META {
                let tag = std::mem::take(&mut self.script_tag);
                let mut e = format!("{n}) ");
                if !tag.is_empty() {
                    e.push_str(&tag);
                    e.push(' ');
                }
                e.push_str(content);
                e.push('\n');
                self.scripts.1.push_str(&e);
            }
        } else if name == "textarea" || name == "xmp" {
            self.text(content);
        }
    }
}

/// Appends a numbered metadata entry (`"N) content\n"`).
fn add(list: &mut (usize, String), content: &str) {
    list.0 += 1;
    if list.1.len() < MAX_META {
        let n = list.0;
        let _ = writeln!(list.1, "{n}) {content}");
    }
}

fn has_attributes(content: &str) -> bool {
    content.chars().any(char::is_alphanumeric)
}

/// Lower-case tag name at the start of `s`.
fn tag_name(s: &str) -> String {
    s.chars()
        .take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Index of the `>` closing a start tag (quotes respected).
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '>' => return Some(i),
            Some(_) | None => {}
        }
        if i > MAX_PENDING {
            // A runaway quote: give up on quoting.
            return s.find('>');
        }
    }
    None
}

/// ASCII case-insensitive search.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| {
        h.get(i..i + n.len())
            .is_some_and(|w| w.eq_ignore_ascii_case(n))
    })
}

/// Decodes character references (`&amp;`, `&#39;`, `&#x27;` and the
/// common named entities); unknown ones stay as they are.
pub(crate) fn decode_entities(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('&') {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or_default());
        let after = rest.get(amp + 1..).unwrap_or_default();
        let end = after
            .char_indices()
            .take(33)
            .find(|&(_, c)| c == ';' || !(c.is_ascii_alphanumeric() || c == '#'))
            .map(|(i, _)| i);
        let (name, consumed) = match end {
            Some(e) if after.get(e..).is_some_and(|r| r.starts_with(';')) => {
                (after.get(..e).unwrap_or_default(), e + 1)
            }
            Some(e) => (after.get(..e).unwrap_or_default(), e),
            None => (after, after.len()),
        };
        if let Some(c) = entity(name) {
            out.push(c);
            rest = after.get(consumed..).unwrap_or_default();
        } else {
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

fn entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let v = if let Some(hex) = num.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return Some(match v {
            0 => char::REPLACEMENT_CHARACTER,
            // Windows-1252 C1 code points, as browsers do.
            0x80..=0x9F => {
                let b = [u8::try_from(v).ok()?];
                crate::util::cp1252(&b).chars().next()?
            }
            _ => char::from_u32(v).unwrap_or(char::REPLACEMENT_CHARACTER),
        });
    }
    Some(match name {
        "amp" | "AMP" => '&',
        "lt" | "LT" => '<',
        "gt" | "GT" => '>',
        "quot" | "QUOT" => '"',
        "apos" => '\'',
        "nbsp" => '\u{A0}',
        "copy" | "COPY" => '©',
        "reg" | "REG" => '®',
        "trade" => '™',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "sect" => '§',
        "para" => '¶',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "middot" => '·',
        "bull" => '•',
        "hellip" => '…',
        "ndash" => '–',
        "mdash" => '—',
        "lsquo" => '‘',
        "rsquo" => '’',
        "sbquo" => '‚',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "laquo" => '«',
        "raquo" => '»',
        "iexcl" => '¡',
        "iquest" => '¿',
        "shy" => '\u{AD}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "zwnj" => '\u{200C}',
        "zwj" => '\u{200D}',
        "auml" => 'ä',
        "Auml" => 'Ä',
        "ouml" => 'ö',
        "Ouml" => 'Ö',
        "uuml" => 'ü',
        "Uuml" => 'Ü',
        "szlig" => 'ß',
        "aring" => 'å',
        "Aring" => 'Å',
        "aelig" => 'æ',
        "AElig" => 'Æ',
        "oslash" => 'ø',
        "Oslash" => 'Ø',
        "eacute" => 'é',
        "Eacute" => 'É',
        "egrave" => 'è',
        "Egrave" => 'È',
        "ecirc" => 'ê',
        "euml" => 'ë',
        "aacute" => 'á',
        "Aacute" => 'Á',
        "agrave" => 'à',
        "Agrave" => 'À',
        "acirc" => 'â',
        "atilde" => 'ã',
        "iacute" => 'í',
        "igrave" => 'ì',
        "icirc" => 'î',
        "iuml" => 'ï',
        "oacute" => 'ó',
        "Oacute" => 'Ó',
        "ograve" => 'ò',
        "ocirc" => 'ô',
        "otilde" => 'õ',
        "uacute" => 'ú',
        "ugrave" => 'ù',
        "ucirc" => 'û',
        "ntilde" => 'ñ',
        "Ntilde" => 'Ñ',
        "ccedil" => 'ç',
        "Ccedil" => 'Ç',
        "yacute" => 'ý',
        "yuml" => 'ÿ',
        "thorn" => 'þ',
        "eth" => 'ð',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "micro" => 'µ',
        "not" => '¬',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "hearts" => '♥',
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "pi" => 'π',
        "sigma" => 'σ',
        "omega" => 'ω',
        "mu" => 'μ',
        "infin" => '∞',
        "ne" => '≠',
        "le" => '≤',
        "ge" => '≥',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringSink;

    fn render(html: &str) -> (String, Metadata) {
        let src = pm_core::io::MemSource::new(html.as_bytes().to_vec());
        let mut sink = StringSink::default();
        let m = extract(&src, &mut sink).unwrap();
        (sink.text, m)
    }

    #[test]
    fn renders_blocks_entities_and_tables() {
        let (t, _) = render(
            "<html><head><title>T</title><style>p{}</style></head><body>\
             <p>Hello&nbsp;<b>wor</b>ld &amp; more</p><div>a<br>b</div>\
             <table><tr><td>c1</td><td>c2</td></tr><tr><td>d</td></tr></table>\
             <ul><li>one</li><li>two</li></ul>&#x41;&#66;&bogus;</body></html>",
        );
        assert!(t.contains("T\n"), "{t:?}");
        assert!(t.contains("Hello world & more"), "{t:?}");
        assert!(t.contains("a\nb"), "{t:?}");
        assert!(t.contains("c1\tc2\n"), "{t:?}");
        assert!(t.contains("* one\n* two"), "{t:?}");
        assert!(t.contains("AB&bogus;"), "{t:?}");
        assert!(!t.contains("p{}"), "{t:?}");
    }

    #[test]
    fn autopsy_metadata_lists() {
        let (t, m) = render(
            "<script src=\"x.js\"></script><script>var a=1;</script>\
             <a href=\"http://example.com\">link</a><img src=\"i.png\"/>\
             <!-- secret --><div class=\"c\">text</div>",
        );
        assert_eq!(m["Scripts"], "\n1)  src=\"x.js\" \n2) var a=1;\n");
        assert_eq!(m["Links"], "\n1)  href=\"http://example.com\"\n");
        assert_eq!(m["Images"], "\n1)  src=\"i.png\"\n");
        assert_eq!(m["Comments"], "\n1)  secret \n");
        assert_eq!(m["Others"], "\n1) div: class=\"c\"\n");
        assert!(t.contains("link"));
        assert!(!t.contains("var a"));
    }

    #[test]
    fn pre_keeps_whitespace() {
        let (t, _) = render("<pre>a  b\n c</pre>x   y");
        assert!(t.contains("a  b\n c"), "{t:?}");
        assert!(t.contains("x y"), "{t:?}");
    }

    #[test]
    fn streaming_splits_are_invisible() {
        let body = "<p>word &amp; word</p><!-- c -->".repeat(5_000);
        let (t, m) = render(&body);
        assert_eq!(t.matches("word & word").count(), 5_000);
        assert!(m["Comments"].contains("5000)  c "));
    }

    #[test]
    fn charset_from_meta() {
        let mut html = b"<meta charset=\"iso-8859-1\"><p>K\xF6ln</p>".to_vec();
        html.extend(b"");
        let src = pm_core::io::MemSource::new(html);
        let mut sink = StringSink::default();
        extract(&src, &mut sink).unwrap();
        assert!(sink.text.contains("Köln"));
    }

    #[test]
    fn garbage_does_not_panic() {
        for s in [
            "<",
            "<<<>>>",
            "</",
            "<a href='",
            "<!--",
            "<script>",
            "&#xFFFFFFFF;",
            "&",
        ] {
            let _ = render(s);
        }
    }
}

//! A small, strict XML reader.
//!
//! Tika uses the JDK's Xerces SAX parser for three jobs: reading
//! `tika-mimetypes.xml`, finding the root element of XML documents
//! (`XmlRootExtractor`) and scanning OOXML/StarOffice package parts. The
//! detection result depends on *where Xerces gives up*, so this reader
//! enforces the well-formedness rules that matter before and inside the
//! first start tags: prolog content, the XML declaration, comments,
//! processing instructions, DOCTYPE, attribute syntax, entity references,
//! duplicate attributes and namespace binding. It reports start and end
//! tags (with resolved namespaces) and skips character data.

use std::collections::HashMap;

/// Why reading stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum XmlError {
    /// Not well-formed (Xerces `SAXParseException`).
    Malformed,
    /// The text ended where the document needs more.
    Eof,
    /// The bytes are not valid in the document's encoding (Xerces
    /// `CharConversionException`).
    BadEncoding,
}

/// One attribute of a start tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attr {
    pub(crate) qname: String,
    pub(crate) value: String,
}

impl Attr {
    /// The local part of the name.
    pub(crate) fn local(&self) -> &str {
        local_part(&self.qname)
    }
}

/// A start tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartTag {
    pub(crate) qname: String,
    pub(crate) attrs: Vec<Attr>,
    /// Namespace URI of the element (empty when none).
    pub(crate) ns: String,
    pub(crate) empty: bool,
}

impl StartTag {
    pub(crate) fn local(&self) -> &str {
        local_part(&self.qname)
    }

    pub(crate) fn attr(&self, qname: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.qname == qname)
            .map(|a| a.value.as_str())
    }
}

fn local_part(q: &str) -> &str {
    q.rfind(':').map_or(q, |i| &q[i + 1..])
}

/// A reader event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Start(StartTag),
    End(String),
    /// Character data (only reported when requested).
    Text(String),
}

fn is_name_start(c: char) -> bool {
    matches!(c, ':' | '_' | 'A'..='Z' | 'a'..='z')
        || matches!(c as u32,
            0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x2FF | 0x370..=0x37D | 0x37F..=0x1FFF
            | 0x200C..=0x200D | 0x2070..=0x218F | 0x2C00..=0x2FEF | 0x3001..=0xD7FF
            | 0xF900..=0xFDCF | 0xFDF0..=0xFFFD | 0x10000..=0xEFFFF)
}

fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}')
        || matches!(c as u32, 0x300..=0x36F | 0x203F..=0x2040)
}

fn is_xml_char(c: char) -> bool {
    matches!(c as u32, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x0010_FFFF)
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// Nesting limit for entity references inside entity values.
const MAX_ENTITY_DEPTH: u32 = 16;
/// Total replacement text one reference may expand to (a billion-laughs
/// guard; Xerces fails such documents too).
const MAX_ENTITY_TEXT: usize = 64 * 1024;

/// The five predefined entities.
fn predefined(name: &str) -> Option<&'static str> {
    Some(match name {
        "lt" => "<",
        "gt" => ">",
        "amp" => "&",
        "quot" => "\"",
        "apos" => "'",
        _ => return None,
    })
}

/// An entity value literal with its character references expanded
/// (general entity references stay, to be expanded where the entity is
/// used).
fn expand_char_refs(literal: &str) -> Result<String, XmlError> {
    let mut out = String::with_capacity(literal.len());
    let mut rest = literal;
    while let Some(i) = rest.find("&#") {
        out.push_str(rest.get(..i).unwrap_or_default());
        let after = rest.get(i + 2..).unwrap_or_default();
        let end = after.find(';').ok_or(XmlError::Malformed)?;
        let digits = after.get(..end).unwrap_or_default();
        let v = match digits.strip_prefix('x') {
            Some(h) => u32::from_str_radix(h, 16),
            None => digits.parse(),
        }
        .map_err(|_| XmlError::Malformed)?;
        let c = char::from_u32(v)
            .filter(|c| is_xml_char(*c))
            .ok_or(XmlError::Malformed)?;
        out.push(c);
        rest = after.get(end + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    Ok(out)
}

/// Decoded document text plus whether decoding stopped at bad bytes.
#[derive(Debug)]
pub(crate) struct Decoded {
    pub(crate) text: String,
    /// Byte offset (in the input) of the first undecodable byte.
    pub(crate) bad_at: Option<usize>,
}

fn decode_utf8(b: &[u8], start: usize) -> Decoded {
    let rest = b.get(start..).unwrap_or_default();
    match std::str::from_utf8(rest) {
        Ok(s) => Decoded {
            text: s.to_owned(),
            bad_at: None,
        },
        Err(e) => {
            let ok = e.valid_up_to();
            let text = String::from_utf8_lossy(rest.get(..ok).unwrap_or_default()).into_owned();
            // An incomplete sequence at the very end is not an error yet:
            // Xerces only sees it when it reads past it (end of stream
            // inside a character is reported as malformed too).
            Decoded {
                text,
                bad_at: Some(start + ok),
            }
        }
    }
}

fn decode_utf16(b: &[u8], start: usize, big_endian: bool) -> Decoded {
    let rest = b.get(start..).unwrap_or_default();
    let units = rest.chunks_exact(2).map(|c| {
        let (x, y) = (
            c.first().copied().unwrap_or(0),
            c.get(1).copied().unwrap_or(0),
        );
        if big_endian {
            u16::from_be_bytes([x, y])
        } else {
            u16::from_le_bytes([x, y])
        }
    });
    let mut text = String::new();
    let mut consumed = 0usize;
    for r in char::decode_utf16(units) {
        match r {
            Ok(c) => {
                text.push(c);
                consumed += c.len_utf16() * 2;
            }
            Err(_) => {
                return Decoded {
                    text,
                    bad_at: Some(start + consumed),
                };
            }
        }
    }
    Decoded { text, bad_at: None }
}

fn decode_latin1(b: &[u8], start: usize) -> Decoded {
    Decoded {
        text: b
            .get(start..)
            .unwrap_or_default()
            .iter()
            .map(|&c| char::from(c))
            .collect(),
        bad_at: None,
    }
}

fn decode_ascii(b: &[u8], start: usize) -> Decoded {
    let rest = b.get(start..).unwrap_or_default();
    let ok = rest.iter().position(|&c| c >= 0x80);
    let end = ok.unwrap_or(rest.len());
    Decoded {
        text: rest
            .get(..end)
            .unwrap_or_default()
            .iter()
            .map(|&c| char::from(c))
            .collect(),
        bad_at: ok.map(|o| start + o),
    }
}

/// The encoding named in an XML declaration at the start of ASCII-
/// compatible text.
fn declared_encoding(b: &[u8]) -> Option<String> {
    let head = b.get(..b.len().min(1024))?;
    if !head.starts_with(b"<?xml") {
        return None;
    }
    let end = head.windows(2).position(|w| w == b"?>")?;
    let decl = String::from_utf8_lossy(head.get(..end)?).into_owned();
    let i = decl.find("encoding")?;
    let after = decl
        .get(i + 8..)?
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let q = after.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let v = after.get(1..)?;
    Some(v.get(..v.find(q)?)?.to_ascii_lowercase())
}

/// Decodes a document the way Xerces' entity scanner picks a reader.
/// `None`: an encoding Xerces cannot read (EBCDIC, UCS-4).
pub(crate) fn decode(b: &[u8]) -> Option<Decoded> {
    match b {
        [0xEF, 0xBB, 0xBF, ..] => Some(decode_utf8(b, 3)),
        [0xFE, 0xFF, ..] => Some(decode_utf16(b, 2, true)),
        [0xFF, 0xFE, ..] => Some(decode_utf16(b, 2, false)),
        [0x00, 0x3C, 0x00, 0x3F, ..] => Some(decode_utf16(b, 0, true)),
        [0x3C, 0x00, 0x3F, 0x00, ..] => Some(decode_utf16(b, 0, false)),
        [0x4C, 0x6F, 0xA7, 0x94, ..]
        | [0x00, 0x00, 0x00, 0x3C, ..]
        | [0x3C, 0x00, 0x00, 0x00, ..] => None,
        _ => match declared_encoding(b).as_deref() {
            None | Some("utf-8" | "utf8") => Some(decode_utf8(b, 0)),
            Some("us-ascii" | "ascii") => Some(decode_ascii(b, 0)),
            // A document labelled UTF-16 without a BOM or UTF-16 pattern.
            Some(e) if e.starts_with("utf-16") || e.starts_with("ucs") => None,
            // ISO-8859-x, windows-125x and friends: ASCII-compatible; the
            // names that matter for detection are ASCII.
            Some(_) => Some(decode_latin1(b, 0)),
        },
    }
}

/// Reader state.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    s: &'a str,
    pos: usize,
    /// More input would follow (decoding stopped at bad bytes).
    truncated: bool,
    /// Element names currently open.
    stack: Vec<String>,
    /// Namespace scopes (prefix → URI); `""` is the default namespace.
    ns: Vec<HashMap<String, String>>,
    namespace_aware: bool,
    report_text: bool,
    seen_root: bool,
    root_closed: bool,
    started: bool,
    /// General entities declared in the internal subset: the replacement
    /// text of internal ones (character references expanded), `None` for
    /// external ones. The first declaration of a name wins.
    entities: HashMap<String, Option<String>>,
    /// Undeclared entities are tolerated (external subset or PE refs).
    lenient_entities: bool,
}

impl<'a> Reader<'a> {
    /// A reader over decoded text. `truncated`: the text was cut at
    /// undecodable bytes (reading past the end reports
    /// [`XmlError::BadEncoding`]).
    pub(crate) fn new(s: &'a str, truncated: bool, namespace_aware: bool) -> Self {
        Self {
            s,
            pos: 0,
            truncated,
            stack: Vec::new(),
            ns: Vec::new(),
            namespace_aware,
            report_text: false,
            seen_root: false,
            root_closed: false,
            started: false,
            entities: HashMap::new(),
            lenient_entities: false,
        }
    }

    /// Also report character data (entity references expanded).
    pub(crate) fn with_text(mut self) -> Self {
        self.report_text = true;
        self
    }

    fn rest(&self) -> &'a str {
        self.s.get(self.pos..).unwrap_or_default()
    }

    fn eof(&self) -> XmlError {
        if self.truncated {
            XmlError::BadEncoding
        } else {
            XmlError::Eof
        }
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Result<char, XmlError> {
        let c = self.peek().ok_or_else(|| self.eof())?;
        self.pos += c.len_utf8();
        Ok(c)
    }

    fn starts(&self, p: &str) -> bool {
        self.rest().starts_with(p)
    }

    /// Like `starts`, but distinguishes "needs more input".
    fn expect(&mut self, p: &str) -> Result<(), XmlError> {
        let r = self.rest();
        if r.starts_with(p) {
            self.pos += p.len();
            Ok(())
        } else if p.starts_with(r) {
            Err(self.eof())
        } else {
            Err(XmlError::Malformed)
        }
    }

    fn skip_ws(&mut self) -> bool {
        let r = self.rest();
        let n = r.len() - r.trim_start_matches(is_ws).len();
        self.pos += n;
        n > 0
    }

    fn name(&mut self) -> Result<&'a str, XmlError> {
        let start = self.pos;
        let c = self.peek().ok_or_else(|| self.eof())?;
        if !is_name_start(c) {
            return Err(XmlError::Malformed);
        }
        self.pos += c.len_utf8();
        while let Some(c) = self.peek() {
            if !is_name_char(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
        if self.pos >= self.s.len() {
            // The name might continue.
            return Err(self.eof());
        }
        Ok(self.s.get(start..self.pos).unwrap_or_default())
    }

    /// Reads up to (and consumes) `end`; checks characters.
    fn until(&mut self, end: &str) -> Result<&'a str, XmlError> {
        let r = self.rest();
        let Some(i) = r.find(end) else {
            // The body may still be invalid; check what we have.
            if r.chars().any(|c| !is_xml_char(c)) {
                return Err(XmlError::Malformed);
            }
            return Err(self.eof());
        };
        let body = r.get(..i).unwrap_or_default();
        if body.chars().any(|c| !is_xml_char(c)) {
            return Err(XmlError::Malformed);
        }
        self.pos += i + end.len();
        Ok(body)
    }

    fn comment(&mut self) -> Result<(), XmlError> {
        self.expect("<!--")?;
        let body = self.until("-->")?;
        if body.contains("--") || body.ends_with('-') {
            return Err(XmlError::Malformed);
        }
        Ok(())
    }

    fn pi(&mut self) -> Result<(), XmlError> {
        self.expect("<?")?;
        let target = self.name()?;
        if target.eq_ignore_ascii_case("xml") {
            // An XML declaration anywhere but the start.
            return Err(XmlError::Malformed);
        }
        if self.starts("?>") {
            self.pos += 2;
            return Ok(());
        }
        if !self.skip_ws() {
            return Err(if self.rest().is_empty() {
                self.eof()
            } else {
                XmlError::Malformed
            });
        }
        self.until("?>").map(|_| ())
    }

    fn quoted(&mut self) -> Result<&'a str, XmlError> {
        let q = self.bump()?;
        if q != '"' && q != '\'' {
            return Err(XmlError::Malformed);
        }
        let r = self.rest();
        let i = r.find(q).ok_or_else(|| self.eof())?;
        self.pos += i + 1;
        Ok(r.get(..i).unwrap_or_default())
    }

    fn xml_decl(&mut self) -> Result<(), XmlError> {
        // `<?xml` followed by whitespace.
        self.expect("<?xml")?;
        if !self.skip_ws() {
            return Err(if self.rest().is_empty() {
                self.eof()
            } else {
                XmlError::Malformed
            });
        }
        let mut seen_version = false;
        let mut idx = 0;
        loop {
            if self.starts("?>") {
                self.pos += 2;
                return if seen_version {
                    Ok(())
                } else {
                    Err(XmlError::Malformed)
                };
            }
            let name = self.name()?;
            self.skip_ws();
            self.expect("=")?;
            self.skip_ws();
            let v = self.quoted()?;
            match (idx, name) {
                (0, "version") => {
                    if !v.starts_with("1.") || v.len() < 3 {
                        return Err(XmlError::Malformed);
                    }
                    seen_version = true;
                }
                (_, "encoding") if seen_version => {
                    if v.is_empty()
                        || !v
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
                    {
                        return Err(XmlError::Malformed);
                    }
                }
                (_, "standalone") if seen_version => {
                    if v != "yes" && v != "no" {
                        return Err(XmlError::Malformed);
                    }
                }
                _ => return Err(XmlError::Malformed),
            }
            idx += 1;
            let ws = self.skip_ws();
            if !ws && !self.starts("?>") {
                return Err(if self.rest().len() < 2 {
                    self.eof()
                } else {
                    XmlError::Malformed
                });
            }
        }
    }

    fn doctype(&mut self) -> Result<(), XmlError> {
        self.expect("<!DOCTYPE")?;
        if !self.skip_ws() {
            return Err(XmlError::Malformed);
        }
        self.name()?;
        let ws = self.skip_ws();
        if ws && (self.starts("SYSTEM") || self.starts("PUBLIC")) {
            let public = self.starts("PUBLIC");
            self.pos += 6;
            if !self.skip_ws() {
                return Err(XmlError::Malformed);
            }
            self.quoted()?;
            if public {
                if !self.skip_ws() {
                    return Err(XmlError::Malformed);
                }
                self.quoted()?;
            }
            self.lenient_entities = true;
            self.skip_ws();
        }
        if self.starts("[") {
            self.pos += 1;
            self.internal_subset()?;
            self.skip_ws();
        }
        self.expect(">")
    }

    fn internal_subset(&mut self) -> Result<(), XmlError> {
        loop {
            self.skip_ws();
            let r = self.rest();
            if r.is_empty() {
                return Err(self.eof());
            }
            if r.starts_with(']') {
                self.pos += 1;
                return Ok(());
            }
            if r.starts_with("<!--") {
                self.comment()?;
            } else if r.starts_with("<?") {
                self.pi()?;
            } else if r.starts_with('%') {
                self.pos += 1;
                self.name()?;
                self.expect(";")?;
                self.lenient_entities = true;
            } else if r.starts_with("<!") {
                self.pos += 2;
                let kw = self.name()?;
                if !matches!(kw, "ELEMENT" | "ATTLIST" | "ENTITY" | "NOTATION") {
                    return Err(XmlError::Malformed);
                }
                if kw == "ENTITY" {
                    self.skip_ws();
                    if !self.starts("%") {
                        let n = self.name()?;
                        self.skip_ws();
                        let value = if self.starts("\"") || self.starts("'") {
                            Some(expand_char_refs(self.quoted()?)?)
                        } else {
                            None
                        };
                        self.entities.entry(n.to_owned()).or_insert(value);
                    }
                }
                // Skip to the closing '>' outside quotes.
                loop {
                    let c = self.bump()?;
                    match c {
                        '"' | '\'' => {
                            let r = self.rest();
                            let i = r.find(c).ok_or_else(|| self.eof())?;
                            self.pos += i + 1;
                        }
                        '>' => break,
                        _ => {}
                    }
                }
            } else {
                return Err(XmlError::Malformed);
            }
        }
    }

    /// Expands one reference after `&`; returns its text.
    fn reference(&mut self, in_attr: bool) -> Result<String, XmlError> {
        if self.starts("#x") {
            self.pos += 2;
            let r = self.rest();
            let end = r.find(';').ok_or_else(|| self.eof())?;
            let v = u32::from_str_radix(r.get(..end).unwrap_or_default(), 16)
                .map_err(|_| XmlError::Malformed)?;
            self.pos += end + 1;
            let c = char::from_u32(v)
                .filter(|c| is_xml_char(*c))
                .ok_or(XmlError::Malformed)?;
            return Ok(c.to_string());
        }
        if self.starts("#") {
            self.pos += 1;
            let r = self.rest();
            let end = r.find(';').ok_or_else(|| self.eof())?;
            let v: u32 = r
                .get(..end)
                .unwrap_or_default()
                .parse()
                .map_err(|_| XmlError::Malformed)?;
            self.pos += end + 1;
            let c = char::from_u32(v)
                .filter(|c| is_xml_char(*c))
                .ok_or(XmlError::Malformed)?;
            return Ok(c.to_string());
        }
        let n = self.name()?;
        self.expect(";")?;
        if let Some(c) = predefined(n) {
            return Ok(c.to_owned());
        }
        let mut budget = MAX_ENTITY_TEXT;
        self.entity_text(n, in_attr, 0, &mut budget)
    }

    /// The replacement text of general entity `name`, nested references
    /// expanded (an undeclared or external entity gives nothing when the
    /// document has an external subset or parameter-entity references,
    /// which Xerces does not read for detection). In an attribute value,
    /// markup in the replacement text is an error.
    fn entity_text(
        &self,
        name: &str,
        in_attr: bool,
        depth: u32,
        budget: &mut usize,
    ) -> Result<String, XmlError> {
        let value = match self.entities.get(name) {
            Some(Some(v)) => v,
            Some(None) => return Ok(String::new()),
            None if self.lenient_entities => return Ok(String::new()),
            None => return Err(XmlError::Malformed),
        };
        // Xerces rejects recursive entities and caps expansion.
        if depth > MAX_ENTITY_DEPTH || (in_attr && value.contains('<')) {
            return Err(XmlError::Malformed);
        }
        *budget = budget.checked_sub(value.len()).ok_or(XmlError::Malformed)?;
        let mut out = String::with_capacity(value.len());
        let mut rest = value.as_str();
        while let Some(i) = rest.find('&') {
            out.push_str(rest.get(..i).unwrap_or_default());
            let after = rest.get(i + 1..).unwrap_or_default();
            let end = after.find(';').ok_or(XmlError::Malformed)?;
            let inner = after.get(..end).unwrap_or_default();
            match predefined(inner) {
                Some(c) => out.push_str(c),
                None => out.push_str(&self.entity_text(inner, in_attr, depth + 1, budget)?),
            }
            rest = after.get(end + 1..).unwrap_or_default();
        }
        out.push_str(rest);
        Ok(out)
    }

    fn attr_value(&mut self) -> Result<String, XmlError> {
        let q = self.bump()?;
        if q != '"' && q != '\'' {
            return Err(XmlError::Malformed);
        }
        let mut v = String::new();
        loop {
            let c = self.bump()?;
            match c {
                c if c == q => return Ok(v),
                '<' => return Err(XmlError::Malformed),
                '&' => {
                    let char_ref = self.starts("#");
                    let text = self.reference(true)?;
                    if char_ref {
                        v.push_str(&text);
                    } else {
                        // Entity text is normalised like literal text.
                        v.extend(text.chars().map(|c| if is_ws(c) { ' ' } else { c }));
                    }
                }
                '\t' | '\n' | '\r' => v.push(' '),
                c if !is_xml_char(c) => return Err(XmlError::Malformed),
                c => v.push(c),
            }
        }
    }

    fn start_tag(&mut self) -> Result<StartTag, XmlError> {
        self.expect("<")?;
        let qname = self.name()?.to_owned();
        let mut attrs: Vec<Attr> = Vec::new();
        let empty;
        loop {
            let ws = self.skip_ws();
            if self.starts("/>") {
                self.pos += 2;
                empty = true;
                break;
            }
            if self.starts(">") {
                self.pos += 1;
                empty = false;
                break;
            }
            if self.rest().is_empty() || self.rest() == "/" {
                return Err(self.eof());
            }
            if !ws {
                return Err(XmlError::Malformed);
            }
            let name = self.name()?.to_owned();
            self.skip_ws();
            self.expect("=")?;
            self.skip_ws();
            let value = self.attr_value()?;
            if attrs.iter().any(|a| a.qname == name) {
                return Err(XmlError::Malformed);
            }
            attrs.push(Attr { qname: name, value });
        }
        let mut ns = String::new();
        if self.namespace_aware {
            let mut scope = HashMap::new();
            for a in &attrs {
                if a.qname == "xmlns" {
                    scope.insert(String::new(), a.value.clone());
                } else if let Some(p) = a.qname.strip_prefix("xmlns:") {
                    if a.value.is_empty() {
                        return Err(XmlError::Malformed);
                    }
                    scope.insert(p.to_owned(), a.value.clone());
                }
            }
            self.ns.push(scope);
            let prefix = qname.rfind(':').map_or("", |i| &qname[..i]);
            if qname.starts_with(':') || qname.ends_with(':') || qname.matches(':').count() > 1 {
                return Err(XmlError::Malformed);
            }
            ns = match self.lookup(prefix) {
                Some(u) => u,
                None if prefix.is_empty() => String::new(),
                None if prefix == "xml" => "http://www.w3.org/XML/1998/namespace".into(),
                None => return Err(XmlError::Malformed),
            };
            for a in &attrs {
                if let Some(i) = a.qname.rfind(':') {
                    let p = &a.qname[..i];
                    if p != "xmlns" && p != "xml" && self.lookup(p).is_none() {
                        return Err(XmlError::Malformed);
                    }
                }
            }
            if empty {
                self.ns.pop();
            }
        }
        if !empty {
            self.stack.push(qname.clone());
        }
        Ok(StartTag {
            qname,
            attrs,
            ns,
            empty,
        })
    }

    /// The URI bound to `prefix` (`""` for an undeclared default
    /// namespace via `xmlns=""`).
    fn lookup(&self, prefix: &str) -> Option<String> {
        self.ns.iter().rev().find_map(|s| s.get(prefix).cloned())
    }

    /// The next event; `Ok(None)` at a clean end of the document.
    pub(crate) fn next(&mut self) -> Result<Option<Event>, XmlError> {
        if !self.started {
            self.started = true;
            if self.starts("<?xml") && self.rest().get(5..6).is_some_and(|c| c.chars().all(is_ws)) {
                self.xml_decl()?;
            }
        }
        loop {
            if self.stack.is_empty() {
                // Prolog or epilog: whitespace and markup only.
                self.skip_ws();
                let r = self.rest();
                if r.is_empty() {
                    return if self.root_closed {
                        Ok(None)
                    } else {
                        Err(self.eof())
                    };
                }
                if r.starts_with("<!--") {
                    self.comment()?;
                } else if r.starts_with("<?") {
                    self.pi()?;
                } else if r.starts_with("<!DOCTYPE") && !self.seen_root {
                    self.doctype()?;
                } else if r.starts_with("<!") && "<!DOCTYPE".starts_with(r) {
                    return Err(self.eof());
                } else if r.starts_with('<') && !self.seen_root {
                    if r.len() == 1 {
                        return Err(self.eof());
                    }
                    let t = self.start_tag()?;
                    self.seen_root = true;
                    if t.empty {
                        self.root_closed = true;
                    }
                    return Ok(Some(Event::Start(t)));
                } else {
                    return Err(XmlError::Malformed);
                }
                continue;
            }
            let r = self.rest();
            if r.is_empty() {
                return Err(self.eof());
            }
            if r.starts_with("</") {
                self.pos += 2;
                let n = self.name()?.to_owned();
                self.skip_ws();
                self.expect(">")?;
                if self.stack.pop().as_deref() != Some(n.as_str()) {
                    return Err(XmlError::Malformed);
                }
                if self.namespace_aware {
                    self.ns.pop();
                }
                if self.stack.is_empty() {
                    self.root_closed = true;
                }
                return Ok(Some(Event::End(n)));
            }
            if r.starts_with("<!--") {
                self.comment()?;
                continue;
            }
            if r.starts_with("<![CDATA[") {
                self.pos += 9;
                let body = self.until("]]>")?;
                if self.report_text && !body.is_empty() {
                    return Ok(Some(Event::Text(body.to_owned())));
                }
                continue;
            }
            if r.starts_with("<?") {
                self.pi()?;
                continue;
            }
            if r.starts_with("<!") {
                return Err(if r.len() < 9 {
                    self.eof()
                } else {
                    XmlError::Malformed
                });
            }
            if r.starts_with('<') {
                let t = self.start_tag()?;
                return Ok(Some(Event::Start(t)));
            }
            // Character data up to the next markup.
            let mut text = String::new();
            loop {
                match self.peek() {
                    None => return Err(self.eof()),
                    Some('<') => break,
                    Some('&') => {
                        self.pos += 1;
                        let v = self.reference(false)?;
                        text.push_str(&v);
                    }
                    Some(c) => {
                        if !is_xml_char(c) {
                            return Err(XmlError::Malformed);
                        }
                        if c == ']' && self.starts("]]>") {
                            return Err(XmlError::Malformed);
                        }
                        self.pos += c.len_utf8();
                        if self.report_text {
                            text.push(c);
                        }
                    }
                }
            }
            if self.report_text && !text.is_empty() {
                return Ok(Some(Event::Text(text)));
            }
        }
    }
}

/// Xerces reads ahead this many bytes before the root element is seen; an
/// encoding error in that window aborts the parse.
const READ_AHEAD: usize = 8192;

/// Root element of a parse of `data`, or the encoding error that stopped
/// Xerces.
fn root_once(data: &[u8]) -> Result<Option<(String, String)>, XmlError> {
    let Some(d) = decode(data) else {
        return Ok(None);
    };
    if let Some(bad) = d.bad_at
        && bad < READ_AHEAD.min(data.len())
    {
        return Err(XmlError::BadEncoding);
    }
    let mut r = Reader::new(&d.text, d.bad_at.is_some(), true);
    match r.next() {
        Ok(Some(Event::Start(t))) => {
            let local = t.local().to_owned();
            Ok(Some((t.ns, local)))
        }
        Ok(_) | Err(XmlError::Malformed | XmlError::Eof) => Ok(None),
        Err(XmlError::BadEncoding) => Err(XmlError::BadEncoding),
    }
}

/// Tika's `XmlRootExtractor.extractRootElement(byte[])`: the namespace URI
/// and local name of the root element, retrying with half the data while
/// the bytes are not valid in the document's encoding.
pub(crate) fn root_element(data: &[u8]) -> Option<(String, String)> {
    let mut data = data;
    loop {
        if let Ok(r) = root_once(data) {
            return r;
        }
        let mut n = data.len() / 2;
        if n % 2 == 1 {
            n -= 1;
        }
        if n == 0 {
            return None;
        }
        data = data.get(..n)?;
    }
}

/// `extractRootElement(InputStream)`: no retry on encoding errors.
pub(crate) fn root_element_stream(data: &[u8]) -> Option<(String, String)> {
    root_once(data).ok().flatten()
}

/// Calls `f` for every start tag of a namespace-aware parse until it
/// returns `true` or the document ends or is malformed (a SAX handler that
/// stops early).
pub(crate) fn scan_start_tags(data: &[u8], mut f: impl FnMut(&StartTag) -> bool) {
    let Some(d) = decode(data) else {
        return;
    };
    let mut r = Reader::new(&d.text, d.bad_at.is_some(), true);
    while let Ok(Some(e)) = r.next() {
        if let Event::Start(t) = e
            && f(&t)
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(s: &str) -> Option<(String, String)> {
        root_element(s.as_bytes())
    }

    #[test]
    fn roots() {
        assert_eq!(
            root("<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            Some(("http://www.w3.org/2000/svg".into(), "svg".into()))
        );
        assert_eq!(
            root(
                "<!-- c --><!DOCTYPE html PUBLIC \"a\" \"b\" [<!ENTITY x \"y\">]>\n<a:b xmlns:a='u' c=\"&x;\">"
            ),
            Some(("u".into(), "b".into()))
        );
        assert_eq!(root("<html><body>"), Some((String::new(), "html".into())));
        // Unbound prefix, junk in the prolog, unquoted attributes,
        // undeclared entities, duplicate attributes.
        assert_eq!(root("<a:b>"), None);
        assert_eq!(root("junk<a>"), None);
        assert_eq!(root("<html lang=en>"), None);
        assert_eq!(root("<a b=\"&nbsp;\">"), None);
        assert_eq!(root("<a b='1' b='2'>"), None);
        assert_eq!(root("<a"), None);
        assert_eq!(root(" <?xml version='1.0'?><a/>"), None);
        assert_eq!(
            root("<?xml version='1.0' encoding='ISO-8859-1'?><a/>"),
            Some((String::new(), "a".into()))
        );
        // UTF-16 with a BOM.
        let mut b = vec![0xFF, 0xFE];
        for u in "<x/>".encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(root_element(&b), Some((String::new(), "x".into())));
    }

    /// Internal entities in namespace declarations (RDF/OWL style, Tika's
    /// `test-difficult-rdf*.xml`).
    #[test]
    fn entity_expansion() {
        let rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
        assert_eq!(
            root(&format!(
                "<!DOCTYPE rdf:RDF [<!ENTITY rdf '{rdf}'>]>\n<rdf:RDF xmlns:rdf=\"&rdf;\">"
            )),
            Some((rdf.into(), "RDF".into()))
        );
        // Nested references, character references, first declaration wins.
        assert_eq!(
            root(
                "<!DOCTYPE r [<!ENTITY a 'u&#x3A;'><!ENTITY b '&a;x'><!ENTITY a 'z'>]><r xmlns='&b;#'/>"
            ),
            Some(("u:x#".into(), "r".into()))
        );
        // Markup in an attribute's entity, recursion, a billion laughs.
        assert_eq!(root("<!DOCTYPE r [<!ENTITY a '<b/>'>]><r x='&a;'/>"), None);
        assert_eq!(root("<!DOCTYPE r [<!ENTITY a '&a;'>]><r x='&a;'/>"), None);
        let mut laughs = String::from("<!DOCTYPE r [<!ENTITY l0 'hahahahahahahahahaha'>");
        for i in 1..10 {
            let refs = format!("&l{};", i - 1).repeat(10);
            std::fmt::Write::write_fmt(&mut laughs, format_args!("<!ENTITY l{i} '{refs}'>"))
                .unwrap();
        }
        laughs.push_str("]><r x='&l9;'/>");
        assert_eq!(root(&laughs), None);
        // An external entity gives no text.
        assert_eq!(
            root("<!DOCTYPE r [<!ENTITY e SYSTEM 'e.xml'>]><r xmlns='u&e;'/>"),
            Some(("u".into(), "r".into()))
        );
    }

    #[test]
    fn bad_bytes_halve() {
        // An invalid byte early: halving until it is gone loses the root.
        let mut b = b"<a>".to_vec();
        b.push(0xFF);
        b.extend(std::iter::repeat_n(b' ', 100));
        assert_eq!(root_element(&b), None);
        // Invalid bytes far after the read-ahead window do not matter.
        let mut b = b"<a>".to_vec();
        b.extend(std::iter::repeat_n(b' ', 9000));
        b.push(0xFF);
        assert_eq!(root_element(&b), Some((String::new(), "a".into())));
    }

    #[test]
    fn full_documents() {
        let doc = "<?xml version='1.0'?><r><a x='1'>t&amp;<![CDATA[<>]]></a><b/></r>";
        let mut r = Reader::new(doc, false, false);
        let mut names = Vec::new();
        while let Some(e) = r.next().unwrap() {
            if let Event::Start(t) = e {
                names.push(t.qname);
            }
        }
        assert_eq!(names, ["r", "a", "b"]);
        let mut r = Reader::new("<r><a></b></r>", false, false);
        assert!(r.next().is_ok());
        assert!(r.next().is_ok());
        assert_eq!(r.next(), Err(XmlError::Malformed));
    }
}

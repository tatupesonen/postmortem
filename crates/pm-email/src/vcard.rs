//! vCard 2.1/3.0/4.0 contacts (what Autopsy's `VcardParser` reads with
//! ez-vcard): formatted and structured names, telephone numbers and
//! e-mail addresses with their first type, URLs, organisations and
//! embedded photos.

use base64::Engine as _;

/// One contact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VCard {
    /// `FN`.
    pub formatted_name: Option<String>,
    /// `N`: family, given, additional, prefixes, suffixes.
    pub structured_name: Option<[String; 5]>,
    /// `(first TYPE, number)`.
    pub phones: Vec<(Option<String>, String)>,
    /// `(first TYPE, address)`.
    pub emails: Vec<(Option<String>, String)>,
    pub urls: Vec<String>,
    /// First value of each `ORG`.
    pub organizations: Vec<String>,
    /// Embedded photos (`(type, bytes)`).
    pub photos: Vec<(Option<String>, Vec<u8>)>,
}

impl VCard {
    /// Autopsy's contact name: `FN`, else the structured name as
    /// "prefixes given family suffixes (additional )".
    pub fn display_name(&self) -> String {
        if let Some(f) = self
            .formatted_name
            .as_deref()
            .filter(|f| !f.trim().is_empty())
        {
            return f.to_owned();
        }
        let Some([family, given, additional, prefix, suffix]) = &self.structured_name else {
            return String::new();
        };
        let mut name = String::new();
        for part in [prefix, given, family, suffix] {
            if !part.is_empty() {
                name.push_str(part);
                name.push(' ');
            }
        }
        if !additional.is_empty() {
            name.push('(');
            name.push_str(additional);
            name.push_str(" )");
        }
        name
    }
}

/// Whether text is a vCard (`BEGIN:VCARD`).
pub fn is_vcard(head: &[u8]) -> bool {
    let s = String::from_utf8_lossy(head.get(..head.len().min(256)).unwrap_or(head));
    s.trim_start_matches('\u{feff}')
        .trim_start()
        .to_ascii_uppercase()
        .starts_with("BEGIN:VCARD")
}

/// Unfolds continuation lines (RFC 6350: a line starting with a space or
/// tab continues the previous one; vCard 2.1 quoted-printable soft breaks
/// end in `=`).
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix([' ', '\t'])
            && let Some(last) = out.last_mut()
        {
            last.push_str(rest);
            continue;
        }
        if let Some(last) = out.last_mut()
            && last.ends_with('=')
            && last.to_ascii_uppercase().contains("QUOTED-PRINTABLE")
        {
            last.pop();
            last.push_str(line);
            continue;
        }
        out.push(line.to_owned());
    }
    out
}

fn decode_qp(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b.get(i) == Some(&b'=')
            && let (Some(&h), Some(&l)) = (b.get(i + 1), b.get(i + 2))
            && let Ok(v) = u8::from_str_radix(&String::from_utf8_lossy(&[h, l]), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        if let Some(&c) = b.get(i) {
            out.push(c);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// vCard text escapes.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A property line split into name, parameters and value.
struct Prop {
    name: String,
    types: Vec<String>,
    encoding: Option<String>,
    value: String,
}

fn prop(line: &str) -> Option<Prop> {
    let (head, value) = line.split_once(':')?;
    let mut parts = head.split(';');
    let name = parts.next()?.rsplit('.').next()?.to_ascii_uppercase();
    let mut types = Vec::new();
    let mut encoding = None;
    for p in parts {
        match p.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("TYPE") => {
                types.extend(
                    v.split(',')
                        .map(|t| t.trim_matches('"').to_ascii_uppercase()),
                );
            }
            Some((k, v)) if k.eq_ignore_ascii_case("ENCODING") => {
                encoding = Some(v.to_ascii_uppercase());
            }
            // vCard 2.1 bare types (`TEL;HOME;VOICE:`).
            None => types.push(p.to_ascii_uppercase()),
            Some(_) => {}
        }
    }
    let value = if encoding.as_deref() == Some("QUOTED-PRINTABLE") {
        decode_qp(value)
    } else {
        value.to_owned()
    };
    Some(Prop {
        name,
        types,
        encoding,
        value,
    })
}

/// Every vCard in `text`.
pub fn parse(text: &str) -> Vec<VCard> {
    let mut cards = Vec::new();
    let mut card: Option<VCard> = None;
    for line in unfold(text) {
        let Some(p) = prop(line.trim_start_matches('\u{feff}')) else {
            continue;
        };
        match (p.name.as_str(), card.as_mut()) {
            ("BEGIN", _) if p.value.trim().eq_ignore_ascii_case("VCARD") => {
                card = Some(VCard::default());
            }
            ("END", Some(_)) if p.value.trim().eq_ignore_ascii_case("VCARD") => {
                cards.extend(card.take());
            }
            ("FN", Some(c)) => c.formatted_name = Some(unescape(&p.value)),
            ("N", Some(c)) => {
                let mut f = p.value.split(';').map(unescape);
                c.structured_name = Some(std::array::from_fn(|_| f.next().unwrap_or_default()));
            }
            ("TEL", Some(c)) => {
                let first = p.types.into_iter().find(|t| t != "PREF" && t != "VOICE");
                let number = p.value.trim().trim_start_matches("tel:").to_owned();
                if !number.is_empty() {
                    c.phones.push((first, number));
                }
            }
            ("EMAIL", Some(c)) => {
                let first = p.types.into_iter().find(|t| t != "PREF" && t != "INTERNET");
                let addr = p.value.trim().to_owned();
                if !addr.is_empty() {
                    c.emails.push((first, addr));
                }
            }
            ("URL", Some(c)) => c.urls.push(unescape(p.value.trim())),
            ("ORG", Some(c)) => {
                if let Some(o) = p
                    .value
                    .split(';')
                    .next()
                    .map(unescape)
                    .filter(|o| !o.is_empty())
                {
                    c.organizations.push(o);
                }
            }
            ("PHOTO", Some(c)) => {
                let b64 = p
                    .encoding
                    .as_deref()
                    .is_some_and(|e| e == "B" || e == "BASE64");
                let data = if b64 {
                    Some(p.value.trim())
                } else {
                    p.value.split_once(";base64,").map(|(_, d)| d.trim())
                };
                if let Some(d) = data
                    && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(d)
                {
                    c.photos.push((p.types.into_iter().next(), bytes));
                }
            }
            _ => {}
        }
    }
    cards
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vcards_of_several_versions_parse() {
        let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Doe;John;Q;Dr.;Jr.\r\nTEL;TYPE=CELL,VOICE:+358 40 123\r\nTEL:555-0100\r\nEMAIL;TYPE=INTERNET,WORK:john@example.com\r\nORG:Acme;R&D\r\nURL:https://example.com/\r\n jd\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:2.1\r\nFN;ENCODING=QUOTED-PRINTABLE:J=C3=A4rvi=\r\nnen\r\nTEL;HOME;VOICE:123\r\nEND:VCARD\r\n";
        let cards = parse(text);
        assert_eq!(cards.len(), 2);
        let c = &cards[0];
        assert_eq!(c.display_name(), "Dr. John Doe Jr. (Q )");
        assert_eq!(
            c.phones,
            [
                (Some("CELL".into()), "+358 40 123".into()),
                (None, "555-0100".into())
            ]
        );
        assert_eq!(c.emails, [(Some("WORK".into()), "john@example.com".into())]);
        assert_eq!(c.organizations, ["Acme"]);
        assert_eq!(c.urls, ["https://example.com/jd"]);
        assert_eq!(cards[1].display_name(), "Järvinen");
        assert_eq!(cards[1].phones, [(Some("HOME".into()), "123".into())]);
        assert!(is_vcard(text.as_bytes()));
    }
}

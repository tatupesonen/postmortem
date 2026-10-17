//! Media types and the type registry (Tika `MediaType` and
//! `MediaTypeRegistry`).
//!
//! A media type is kept in Tika's canonical string form: lower-case
//! `type/subtype`, followed by `; key=value` parameters sorted by key.
//! Equality is string equality, exactly like Tika.

use std::collections::{BTreeMap, HashMap};

/// `application/octet-stream`.
pub const OCTET_STREAM: &str = "application/octet-stream";
/// `text/plain`.
pub const TEXT_PLAIN: &str = "text/plain";
/// `text/html`.
pub const TEXT_HTML: &str = "text/html";
/// `application/xml`.
pub const APPLICATION_XML: &str = "application/xml";
/// `application/zip`.
pub const APPLICATION_ZIP: &str = "application/zip";
const EMPTY: &str = "application/x-empty";

/// Tika's `MediaType.isSimpleName`.
fn is_simple_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| matches!(c, b'-' | b'+' | b'.' | b'_' | b'0'..=b'9' | b'a'..=b'z'))
}

/// Characters Tika's `VALID_CHARS` excludes (besides control characters).
fn is_special_or_ws(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '<' | '>' | '@' | ',' | ';' | ':' | '\\' | '"' | '/' | '[' | ']' | '?' | '='
    ) || c.is_whitespace()
}

fn is_token(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| !c.is_control() && !is_special_or_ws(c))
}

/// Builds the canonical string of a type with parameters
/// (`MediaType(String, String, Map)`).
fn build(ty: &str, sub: &str, params: &BTreeMap<String, String>) -> String {
    let mut s = format!(
        "{}/{}",
        ty.trim().to_ascii_lowercase(),
        sub.trim().to_ascii_lowercase()
    );
    for (k, v) in params {
        s.push_str("; ");
        s.push_str(k);
        s.push('=');
        if v.chars().any(is_special_or_ws) {
            s.push('"');
            for c in v.chars() {
                if is_special_or_ws(c) && !c.is_whitespace() {
                    s.push('\\');
                }
                s.push(c);
            }
            s.push('"');
        } else {
            s.push_str(v);
        }
    }
    s
}

fn unquote(mut s: &str) -> &str {
    while let Some(r) = s.strip_prefix('"').or_else(|| s.strip_prefix('\'')) {
        s = r;
    }
    while let Some(r) = s.strip_suffix('"').or_else(|| s.strip_suffix('\'')) {
        s = r;
    }
    s
}

fn parse_params(mut s: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    while !s.is_empty() {
        let (mut key, rest) = match s.find(';') {
            Some(i) => (&s[..i], &s[i + 1..]),
            None => (s, ""),
        };
        s = rest;
        let mut value = "";
        if let Some(eq) = key.find('=') {
            value = &key[eq + 1..];
            key = &key[..eq];
        }
        let key = key.trim();
        if !key.is_empty() {
            out.insert(key.to_ascii_lowercase(), unquote(value.trim()).to_owned());
        }
    }
    out
}

/// Parses a media type like Tika's `MediaType.parse` and returns its
/// canonical string, or `None` when it is not a media type.
pub fn parse(s: &str) -> Option<String> {
    let slash = s.find('/')?;
    if is_simple_name(&s[..slash]) && is_simple_name(&s[slash + 1..]) {
        return Some(s.to_owned());
    }
    // TYPE_PATTERN: (?s)\s*TOKEN\s*/\s*TOKEN\s*($|;.*)
    let (head, params) = match s.find(';') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    if let Some(sl) = head.find('/') {
        let ty = head[..sl].trim();
        let sub = head[sl + 1..].trim();
        if is_token(ty) && is_token(sub) {
            return Some(build(ty, sub, &parse_params(params)));
        }
    }
    // CHARSET_FIRST_PATTERN: charset=x; type/subtype
    let lower = s.trim_start().to_ascii_lowercase();
    if let Some(i) = s.find(';').filter(|_| lower.starts_with("charset")) {
        {
            let (cs, rest) = (&s[..i], s[i + 1..].trim());
            if let Some(sl) = rest.find('/') {
                let ty = rest[..sl].trim();
                let sub = rest[sl + 1..].trim();
                if is_token(ty) && is_token(sub) && cs.contains('=') {
                    return Some(build(ty, sub, &parse_params(cs)));
                }
            }
        }
    }
    None
}

/// The type without parameters (`getBaseType`).
pub fn base(t: &str) -> &str {
    match t.find(';') {
        Some(i) => &t[..i],
        None => t,
    }
}

/// Whether the type has parameters.
pub fn has_params(t: &str) -> bool {
    t.contains(';')
}

/// The part before the slash.
fn top(t: &str) -> &str {
    let b = base(t);
    b.find('/').map_or(b, |i| &b[..i])
}

/// The part after the slash (without parameters).
fn subtype(t: &str) -> &str {
    let b = base(t);
    b.find('/').map_or("", |i| &b[i + 1..])
}

/// Aliases and the type hierarchy (`MediaTypeRegistry`).
#[derive(Debug, Default)]
pub struct Registry {
    /// Canonical type of every known type and alias.
    canonical: HashMap<String, String>,
    /// Explicit super types (`sub-class-of`).
    inheritance: HashMap<String, String>,
}

impl Registry {
    pub(crate) fn add_type(&mut self, t: &str) {
        self.canonical.insert(t.to_owned(), t.to_owned());
    }

    pub(crate) fn add_alias(&mut self, t: &str, alias: &str) {
        self.canonical.insert(alias.to_owned(), t.to_owned());
    }

    pub(crate) fn add_super_type(&mut self, t: &str, parent: &str) {
        self.inheritance.insert(t.to_owned(), parent.to_owned());
    }

    /// Every registered canonical type (Tika `getTypes`), sorted.
    pub fn types(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.canonical.values().map(String::as_str).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Whether `t` (or its alias) is registered.
    pub fn contains(&self, t: &str) -> bool {
        self.canonical.contains_key(t)
    }

    /// Resolves aliases (`normalize`).
    pub fn normalize(&self, t: &str) -> String {
        let b = base(t);
        match self.canonical.get(b) {
            None => t.to_owned(),
            Some(c) if has_params(t) => {
                let mut params = parse_params(&t[b.len() + 1..]);
                // A canonical type never has parameters of its own here.
                let cb = base(c);
                if has_params(c) {
                    for (k, v) in parse_params(&c[cb.len() + 1..]) {
                        params.entry(k).or_insert(v);
                    }
                }
                build(top(cb), subtype(cb), &params)
            }
            Some(c) => c.clone(),
        }
    }

    /// The super type of `t` (`getSupertype`).
    pub fn super_type(&self, t: &str) -> Option<String> {
        if let Some(p) = self.inheritance.get(t) {
            return Some(p.clone());
        }
        if has_params(t) {
            return Some(base(t).to_owned());
        }
        let sub = subtype(t);
        if sub.ends_with("+xml") {
            return Some(APPLICATION_XML.to_owned());
        }
        if sub.ends_with("+zip") {
            return Some(APPLICATION_ZIP.to_owned());
        }
        if top(t) == "text" && t != TEXT_PLAIN {
            return Some(TEXT_PLAIN.to_owned());
        }
        if top(t).contains("empty") && t != EMPTY {
            return Some(EMPTY.to_owned());
        }
        if t != OCTET_STREAM {
            return Some(OCTET_STREAM.to_owned());
        }
        None
    }

    /// Whether `a` is a strict specialization of `b` (`isSpecializationOf`).
    pub fn is_specialization_of(&self, a: &str, b: &str) -> bool {
        let mut cur = self.super_type(a);
        // The hierarchy is finite, but guard against cycles in the data.
        for _ in 0..64 {
            match cur {
                None => return false,
                Some(c) if c == b => return true,
                Some(c) => cur = self.super_type(&c),
            }
        }
        false
    }

    /// `a` equals `b` or specialises it (`isInstanceOf`).
    pub fn is_instance_of(&self, a: &str, b: &str) -> bool {
        a == b || self.is_specialization_of(a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(parse("text/plain").as_deref(), Some("text/plain"));
        assert_eq!(parse("Text/HTML ").as_deref(), Some("text/html"));
        assert_eq!(
            parse("text/plain; charset=UTF-8; a=b").as_deref(),
            Some("text/plain; a=b; charset=UTF-8")
        );
        assert_eq!(
            parse("charset=utf-8; text/plain").as_deref(),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            parse("image/vnd.dgn;version=8").as_deref(),
            Some("image/vnd.dgn; version=8")
        );
        assert_eq!(parse("nonsense"), None);
        assert_eq!(base("audio/ogg; codecs=opus"), "audio/ogg");
    }

    #[test]
    fn hierarchy() {
        let mut r = Registry::default();
        r.add_type("application/zip");
        r.add_type("application/java-archive");
        r.add_super_type("application/java-archive", "application/zip");
        r.add_alias("application/java-archive", "application/x-jar");
        assert!(r.is_specialization_of("application/java-archive", "application/zip"));
        assert!(r.is_specialization_of("application/java-archive", OCTET_STREAM));
        assert!(!r.is_specialization_of("application/zip", "application/zip"));
        assert!(r.is_specialization_of("text/x-java", TEXT_PLAIN));
        assert!(r.is_specialization_of("image/svg+xml", APPLICATION_XML));
        assert!(r.is_specialization_of("text/plain; charset=x", TEXT_PLAIN));
        assert!(!r.is_specialization_of(OCTET_STREAM, OCTET_STREAM));
        assert_eq!(r.normalize("application/x-jar"), "application/java-archive");
        assert_eq!(
            r.normalize("application/x-jar; a=b"),
            "application/java-archive; a=b"
        );
    }
}

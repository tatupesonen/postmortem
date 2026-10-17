//! The Tika type database (`MimeTypes`, `MimeTypesReader`, `Patterns`).

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::magic::{Clause, MagicError, MagicMatch};
use crate::media::{self, Registry};
use crate::text;
use crate::xml::{self, Event, Reader};

/// Apache Tika 3.0.0's `tika-mimetypes.xml`, verbatim.
pub(crate) const TIKA_MIMETYPES: &str = include_str!("../data/tika-mimetypes.xml");

/// Bytes of the file Tika reads for magic matching (`getMinLength`).
pub const MAGIC_PREFIX_LEN: usize = 64 * 1024;

/// A type in the database (Tika `MimeType`).
#[derive(Debug)]
pub(crate) struct MimeType {
    pub(crate) name: String,
    pub(crate) interpreted: bool,
    root_xml: Vec<(String, String)>,
}

impl MimeType {
    /// `RootXML.matches`.
    fn matches_xml(&self, ns: &str, local: &str) -> bool {
        self.root_xml.iter().any(|(n, l)| {
            (if n.is_empty() { ns.is_empty() } else { n == ns })
                && (if l.is_empty() {
                    local.is_empty()
                } else {
                    l == local
                })
        })
    }
}

/// A `<magic>` clause of a type (Tika `Magic`).
#[derive(Debug)]
pub(crate) struct Magic {
    ty: usize,
    priority: i32,
    clause: Clause,
    /// `"[" + priority + "/" + clause + "]"`, the last sort key.
    desc: String,
    size: usize,
}

/// File-name patterns (`Patterns`).
#[derive(Debug, Default)]
struct Patterns {
    names: HashMap<String, usize>,
    extensions: HashMap<String, usize>,
    /// (Java regex source, compiled, type), longest source first.
    globs: Vec<(String, Regex, usize)>,
    min_ext: usize,
    max_ext: usize,
}

/// Number of UTF-16 code units (Java `String.length()`).
fn jlen(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `Patterns.compile`: a glob as a Java regex.
fn glob_to_regex(glob: &str) -> String {
    let mut p = String::from("\\A");
    for c in glob.chars() {
        match c {
            '?' => p.push('.'),
            '*' => p.push_str(".*"),
            c if "\\[]^.-$+(){}|".contains(c) => {
                p.push('\\');
                p.push(c);
            }
            c => p.push(c),
        }
    }
    p.push_str("\\z");
    p
}

/// Name of a type index (octet-stream for an invalid index).
fn name_of(types: &[MimeType], i: usize) -> &str {
    types
        .get(i)
        .map_or(media::OCTET_STREAM, |t| t.name.as_str())
}

/// The type database.
#[derive(Debug)]
pub struct MimeDb {
    pub(crate) registry: Registry,
    pub(crate) types: Vec<MimeType>,
    by_name: HashMap<String, usize>,
    magics: Vec<Magic>,
    /// Types with root-XML rules, sorted by name.
    xmls: Vec<usize>,
    patterns: Patterns,
    /// Magics that failed to compile (empty for the shipped database).
    pub(crate) errors: Vec<MagicError>,
    octet: usize,
    text: usize,
    html: Option<usize>,
}

/// Loader errors.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("type database is not well-formed XML")]
    Xml,
    #[error("conflicting {0} pattern: {1}")]
    Conflict(&'static str, String),
    #[error("invalid media type {0}")]
    BadType(String),
    #[error("invalid glob {0}: {1}")]
    BadGlob(String, String),
}

struct ClauseRecord {
    /// `None` for the `<magic>` root; `Some(Err)` for a placeholder whose
    /// match failed to compile.
    clause: Option<Result<Clause, MagicError>>,
    min_should: Option<usize>,
    subclauses: Vec<Result<Clause, MagicError>>,
}

impl MimeDb {
    /// The shipped database (parsed once, ~10 ms).
    pub fn get() -> &'static Self {
        static DB: OnceLock<MimeDb> = OnceLock::new();
        DB.get_or_init(|| {
            Self::parse(TIKA_MIMETYPES).unwrap_or_else(|e| {
                tracing::error!("cannot load the embedded Tika database: {e}");
                Self::empty()
            })
        })
    }

    fn empty() -> Self {
        let mut db = Self {
            registry: Registry::default(),
            types: Vec::new(),
            by_name: HashMap::new(),
            magics: Vec::new(),
            xmls: Vec::new(),
            patterns: Patterns {
                min_ext: usize::MAX,
                ..Patterns::default()
            },
            errors: Vec::new(),
            octet: 0,
            text: 0,
            html: None,
        };
        db.octet = db.add(media::OCTET_STREAM);
        db.text = db.add(media::TEXT_PLAIN);
        db.add(media::APPLICATION_XML);
        db
    }

    fn add(&mut self, name: &str) -> usize {
        self.registry.add_type(name);
        let i = self.types.len();
        self.types.push(MimeType {
            name: name.to_owned(),
            interpreted: false,
            root_xml: Vec::new(),
        });
        self.by_name.insert(name.to_owned(), i);
        i
    }

    /// `MimeTypes.forName`.
    fn for_name(&mut self, name: &str) -> Result<usize, DbError> {
        let t = media::parse(name).ok_or_else(|| DbError::BadType(name.to_owned()))?;
        let n = self.registry.normalize(&t);
        if let Some(&i) = self.by_name.get(&n) {
            return Ok(i);
        }
        Ok(self.add(&t))
    }

    /// Looks up a type without creating it.
    pub(crate) fn lookup(&self, name: &str) -> Option<usize> {
        let t = media::parse(name)?;
        self.by_name.get(&self.registry.normalize(&t)).copied()
    }

    fn spec(&self, a: usize, b: usize) -> bool {
        self.registry
            .is_specialization_of(self.name(a), self.name(b))
    }

    fn add_pattern(&mut self, pattern: &str, is_regex: bool, ty: usize) -> Result<(), DbError> {
        enum Kind {
            Name(String),
            Ext(String),
            Glob(String),
        }
        let kind = if is_regex {
            Kind::Glob(pattern.to_owned())
        } else if !pattern.contains(['*', '?', '[']) {
            Kind::Name(pattern.to_owned())
        } else if pattern.starts_with('*')
            && !pattern[1..].contains('*')
            && !pattern.contains(['?', '['])
        {
            Kind::Ext(pattern[1..].to_owned())
        } else {
            Kind::Glob(glob_to_regex(pattern))
        };
        let (label, prev) = match &kind {
            Kind::Name(n) => ("name", self.patterns.names.get(n).copied()),
            Kind::Ext(e) => ("extension", self.patterns.extensions.get(e).copied()),
            Kind::Glob(g) => (
                "glob",
                self.patterns
                    .globs
                    .iter()
                    .find(|(s, _, _)| s == g)
                    .map(|(_, _, t)| *t),
            ),
        };
        let replace = match prev {
            None => true,
            Some(p) if self.spec(p, ty) => true,
            Some(p) if p == ty || self.spec(ty, p) => false,
            Some(_) => return Err(DbError::Conflict(label, pattern.to_owned())),
        };
        if !replace {
            return Ok(());
        }
        match kind {
            Kind::Name(n) => {
                self.patterns.names.insert(n, ty);
            }
            Kind::Ext(e) => {
                let l = jlen(&e);
                self.patterns.min_ext = self.patterns.min_ext.min(l);
                self.patterns.max_ext = self.patterns.max_ext.max(l);
                self.patterns.extensions.insert(e, ty);
            }
            Kind::Glob(g) => {
                let re = Regex::new(&format!("^(?:{g})$"))
                    .map_err(|e| DbError::BadGlob(g.clone(), e.to_string()))?;
                self.patterns.globs.retain(|(s, _, _)| *s != g);
                self.patterns.globs.push((g, re, ty));
                // LengthComparator: longer first, then lexicographic.
                self.patterns
                    .globs
                    .sort_by(|a, b| jlen(&b.0).cmp(&jlen(&a.0)).then_with(|| a.0.cmp(&b.0)));
            }
        }
        Ok(())
    }

    /// Parses a `mime-info` document (`MimeTypesReader`).
    pub fn parse(src: &str) -> Result<Self, DbError> {
        let mut db = Self::empty();
        let mut reader = Reader::new(src, false, false);
        let mut cur: Option<usize> = None;
        let mut priority = 50;
        let mut records: Vec<ClauseRecord> = Vec::new();
        let mut magic_clauses: Vec<(usize, i32, Result<Clause, MagicError>)> = Vec::new();
        loop {
            let ev = match reader.next() {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(_) => return Err(DbError::Xml),
            };
            match ev {
                Event::Start(t) => {
                    let Some(ty) = cur else {
                        if t.qname == "mime-type" {
                            let name = t.attr("type").unwrap_or_default().to_owned();
                            let i = db.for_name(&name)?;
                            if let Some(m) = db.types.get_mut(i) {
                                m.interpreted = t.attr("interpreted") == Some("true");
                            }
                            cur = Some(i);
                            if t.empty {
                                cur = None;
                            }
                        }
                        continue;
                    };
                    match t.qname.as_str() {
                        "alias" => {
                            if let Some(a) = t.attr("type").and_then(media::parse) {
                                let canon = db.name(ty).to_owned();
                                db.registry.add_alias(&canon, &a);
                            }
                        }
                        "sub-class-of" => {
                            if let Some(p) = t.attr("type").and_then(media::parse) {
                                let name = db.name(ty).to_owned();
                                db.registry.add_super_type(&name, &p);
                            }
                        }
                        "glob" => {
                            if let Some(p) = t.attr("pattern") {
                                let is_regex = t.attr("isregex") == Some("true");
                                db.add_pattern(p, is_regex, ty)?;
                            }
                        }
                        "root-XML" => {
                            let ns = t.attr("namespaceURI").unwrap_or_default().to_owned();
                            let local = t.attr("localName").unwrap_or_default().to_owned();
                            if let Some(m) = db.types.get_mut(ty) {
                                m.root_xml.push((ns, local));
                            }
                        }
                        "magic" => {
                            priority = match t.attr("priority") {
                                Some(v) if !v.is_empty() => v.parse().map_err(|_| DbError::Xml)?,
                                _ => 50,
                            };
                            records.clear();
                            records.push(ClauseRecord {
                                clause: None,
                                min_should: None,
                                subclauses: Vec::new(),
                            });
                            if t.empty {
                                records.clear();
                            }
                        }
                        "match" => {
                            let rec = if let Some(m) = t.attr("minShouldMatch") {
                                ClauseRecord {
                                    clause: None,
                                    min_should: Some(m.parse().map_err(|_| DbError::Xml)?),
                                    subclauses: Vec::new(),
                                }
                            } else {
                                let kind = t.attr("type").unwrap_or("string");
                                let m = MagicMatch::new(
                                    db.name(ty),
                                    kind,
                                    t.attr("offset"),
                                    t.attr("value"),
                                    t.attr("mask"),
                                )
                                .map(|m| Clause::Match(Box::new(m)));
                                ClauseRecord {
                                    clause: Some(m),
                                    min_should: None,
                                    subclauses: Vec::new(),
                                }
                            };
                            records.push(rec);
                            if t.empty {
                                stop_record(&mut records);
                            }
                        }
                        _ => {}
                    }
                }
                Event::End(name) => {
                    let Some(ty) = cur else { continue };
                    match name.as_str() {
                        "mime-type" => cur = None,
                        "match" => stop_record(&mut records),
                        "magic" => {
                            if let Some(root) = records.pop() {
                                for c in root.subclauses {
                                    magic_clauses.push((ty, priority, c));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::Text(_) => {}
            }
        }
        // `MimeTypes.init`: collect and sort the magics.
        for (ty, priority, c) in magic_clauses {
            match c {
                Ok(clause) => {
                    let desc = format!("[{priority}/{clause}]");
                    let size = clause.size();
                    db.magics.push(Magic {
                        ty,
                        priority,
                        clause,
                        desc,
                        size,
                    });
                }
                Err(e) => db.errors.push(e),
            }
        }
        let types = &db.types;
        db.magics.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| b.size.cmp(&a.size))
                .then_with(|| name_of(types, b.ty).cmp(name_of(types, a.ty)))
                .then_with(|| b.desc.cmp(&a.desc))
        });
        let mut xmls: Vec<usize> = (0..db.types.len())
            .filter(|&i| db.types.get(i).is_some_and(|t| !t.root_xml.is_empty()))
            .collect();
        xmls.sort_by(|&a, &b| db.name(a).cmp(db.name(b)));
        db.xmls = xmls;
        db.html = db.lookup(media::TEXT_HTML);
        Ok(db)
    }

    /// Canonical name of a type index.
    pub(crate) fn name(&self, i: usize) -> &str {
        name_of(&self.types, i)
    }

    /// `MimeTypes.getMimeType(byte[])`: the candidate types from the magic
    /// prefix (most likely first).
    pub(crate) fn magic_types(&self, data: &[u8]) -> Vec<usize> {
        if data.is_empty() {
            return vec![self.octet];
        }
        let mut result: Vec<usize> = Vec::with_capacity(1);
        let mut current: i32 = -1;
        for m in &self.magics {
            if current > 0 && current > m.priority {
                break;
            }
            if m.clause.eval(data) {
                result.push(m.ty);
                current = m.priority;
            }
        }
        if result.is_empty() {
            let t = if text::looks_like_text(data) {
                self.text
            } else {
                self.octet
            };
            return vec![t];
        }
        for slot in &mut result {
            let name = self.name(*slot);
            if name != media::APPLICATION_XML && name != media::TEXT_HTML {
                continue;
            }
            match xml::root_element(data) {
                Some((ns, local)) => {
                    if let Some(&t) = self.xmls.iter().find(|&&t| {
                        self.types
                            .get(t)
                            .is_some_and(|m| m.matches_xml(&ns, &local))
                    }) {
                        *slot = t;
                    }
                }
                None if name == media::APPLICATION_XML => {
                    // TIKA-2419: XML without a root element is HTML if the
                    // HTML magic matches, otherwise text.
                    let is_html = self.html.is_some_and(|h| {
                        self.magics
                            .iter()
                            .filter(|m| m.ty == h)
                            .any(|m| m.clause.eval(data))
                    });
                    *slot = if is_html {
                        self.html.unwrap_or(self.text)
                    } else {
                        self.text
                    };
                }
                None => {}
            }
        }
        result
    }

    /// `Patterns.matches`.
    fn match_name(&self, name: &str) -> Option<usize> {
        if let Some(&t) = self.patterns.names.get(name) {
            return Some(t);
        }
        let p = &self.patterns;
        if p.max_ext > 0 {
            // Suffixes by UTF-16 length, longest first.
            let mut suffixes: Vec<(usize, usize)> = Vec::new();
            let mut units = 0;
            for (i, c) in name.char_indices().rev() {
                units += c.len_utf16();
                suffixes.push((units, i));
            }
            let max = p.max_ext.min(jlen(name));
            for n in (p.min_ext..=max).rev() {
                if let Some(&t) = suffixes
                    .iter()
                    .find(|(u, _)| *u == n)
                    .and_then(|&(_, i)| p.extensions.get(&name[i..]))
                {
                    return Some(t);
                }
            }
        }
        p.globs
            .iter()
            .find(|(_, re, _)| re.is_match(name))
            .map(|(_, _, t)| *t)
    }

    /// `MimeTypes.getMimeType(String)`: the type a file name suggests
    /// (octet-stream when none).
    pub(crate) fn type_for_name(&self, name: &str) -> usize {
        self.match_name(name)
            .or_else(|| self.match_name(&name.to_lowercase()))
            .unwrap_or(self.octet)
    }

    /// `MimeTypes.applyHint`.
    pub(crate) fn apply_hint(&self, possible: Vec<usize>, hint: usize) -> Vec<usize> {
        if possible.is_empty() {
            return vec![hint];
        }
        if possible.iter().any(|&t| t == hint || self.spec(hint, t)) {
            return vec![hint];
        }
        possible
    }

    /// Every registered type (`MediaTypeRegistry.getTypes()`).
    pub fn types(&self) -> Vec<&str> {
        self.registry.types()
    }

    /// The type registry (aliases and hierarchy).
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Number of magic clauses (for tests and diagnostics).
    pub fn magic_count(&self) -> usize {
        self.magics.len()
    }

    #[cfg(test)]
    pub(crate) fn magic_order(&self) -> Vec<(&str, i32, usize)> {
        self.magics
            .iter()
            .map(|m| (self.name(m.ty), m.priority, m.size))
            .collect()
    }
}

/// `ClauseRecord.stop()`: closes the innermost `<match>`.
fn stop_record(records: &mut Vec<ClauseRecord>) {
    let Some(rec) = records.pop() else { return };
    let clause = if let Some(min) = rec.min_should {
        let subs: Result<Vec<Clause>, MagicError> = rec.subclauses.into_iter().collect();
        subs.and_then(|s| {
            if s.is_empty() || min > s.len() || min == 0 {
                Err(MagicError(format!("minShouldMatch {min}")))
            } else {
                Ok(Clause::MinShould(min, s))
            }
        })
    } else {
        let own = rec
            .clause
            .unwrap_or_else(|| Err(MagicError("match without clause".into())));
        if rec.subclauses.is_empty() {
            own
        } else {
            let subs: Result<Vec<Clause>, MagicError> = rec.subclauses.into_iter().collect();
            match (own, subs) {
                (Ok(c), Ok(mut s)) => {
                    let sub = if s.len() == 1 {
                        s.pop().unwrap_or(Clause::Or(Vec::new()))
                    } else {
                        Clause::Or(s)
                    };
                    Ok(Clause::And(vec![c, sub]))
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        }
    };
    if let Some(parent) = records.last_mut() {
        parent.subclauses.push(clause);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_database_loads_completely() {
        let db = MimeDb::get();
        assert!(db.errors.is_empty(), "{:?}", db.errors);
        assert!(db.types.len() > 1600, "{}", db.types.len());
        // Tika 3.0.0 has 847 magic clauses.
        assert_eq!(db.magic_count(), 847);
        // Highest priority first.
        let order = db.magic_order();
        assert!(order.windows(2).all(|w| w[0].1 >= w[1].1));
    }

    #[test]
    fn names() {
        let db = MimeDb::get();
        let t = |n: &str| db.name(db.type_for_name(n)).to_owned();
        assert_eq!(t("a.pdf"), "application/pdf");
        assert_eq!(t("A.PDF"), "application/pdf");
        assert_eq!(t("x.tar.gz"), "application/gzip");
        assert_eq!(t("Makefile"), "text/x-makefile");
        assert_eq!(t("rdf"), "application/rdf+xml");
        assert_eq!(t("nothing"), "application/octet-stream");
    }

    #[test]
    fn registry() {
        let db = MimeDb::get();
        let r = db.registry();
        assert!(r.is_specialization_of(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/x-tika-ooxml"
        ));
        assert!(r.is_specialization_of("application/java-archive", "application/zip"));
        assert_eq!(r.normalize("application/x-gzip"), "application/gzip");
    }
}

//! Web search queries in visited URLs (Autopsy
//! `SearchEngineURLQueryAnalyzer` with its `SEUQAMappings.xml`, embedded
//! verbatim).
//!
//! Semantics kept from Autopsy: an engine matches when the **whole URL**
//! matches `^(.*[./])?<domainSubstring>([./].*)?$` (the substring is a
//! regular expression, unescaped); split tokens are attached to engines
//! by name, so the two "Wiki" engines share theirs; the first token whose
//! plain form occurs in the URL selects the regex to split the URL on;
//! the query is the last piece up to the first `&`, URL-decoded like
//! Java's `URLDecoder` after stray `%` signs are escaped.

use std::sync::OnceLock;

use regex::Regex;

/// The engine definitions shipped with Autopsy.
pub const MAPPINGS_XML: &str = include_str!("../data/SEUQAMappings.xml");

/// One search engine.
#[derive(Debug, Clone)]
pub struct SearchEngine {
    pub name: String,
    pub domain_substring: String,
    /// `(plainToken, regexToken)` in document order.
    pub keys: Vec<(String, String)>,
    domain_re: Option<Regex>,
}

impl SearchEngine {
    /// Whether the engine's domain pattern matches `url`.
    pub fn matches(&self, url: &str) -> bool {
        self.domain_re.as_ref().is_some_and(|r| r.is_match(url))
    }

    /// The query text for this engine in `url` (`""` when none).
    pub fn query(&self, url: &str) -> String {
        let mut x = String::new();
        for (plain, re) in &self.keys {
            if url.contains(plain.as_str()) {
                x = split_value(url, re);
                break;
            }
        }
        decode(&escape_stray_percent(&x))
    }
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=\"");
    let mut search = tag;
    loop {
        let at = search.find(&pat)?;
        // Must be a whole attribute name.
        let ok = at == 0
            || search
                .as_bytes()
                .get(at - 1)
                .is_some_and(u8::is_ascii_whitespace);
        let start = at + pat.len();
        let rest = search.get(start..)?;
        if ok {
            let len = rest.find('"')?;
            return Some(xml_unescape(rest.get(..len)?));
        }
        search = rest;
    }
}

/// Parses engine definitions in `SEUQAMappings.xml` format (comments are
/// skipped; malformed elements are ignored).
pub fn parse_mappings(xml: &str) -> Vec<SearchEngine> {
    // Drop comments.
    let mut text = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("<!--") {
        text.push_str(rest.get(..i).unwrap_or_default());
        rest = rest
            .get(i..)
            .and_then(|r| r.find("-->").and_then(|e| r.get(e + 3..)))
            .unwrap_or_default();
    }
    text.push_str(rest);
    let mut engines: Vec<(String, String)> = Vec::new();
    // (engine name of the parent element, plain, regex)
    let mut tokens: Vec<(String, String, String)> = Vec::new();
    let mut current: Option<String> = None;
    for tag in text.split('<').skip(1) {
        let tag = tag.split('>').next().unwrap_or_default();
        if tag.starts_with("SearchEngine") && !tag.starts_with("SearchEngine/") {
            if let (Some(name), Some(domain)) = (attr(tag, "engine"), attr(tag, "domainSubstring"))
            {
                current = Some(name.clone());
                engines.push((name, domain));
            }
        } else if tag.starts_with("/SearchEngine") {
            current = None;
        } else if tag.starts_with("splitToken")
            && let (Some(engine), Some(plain), Some(re)) = (
                current.clone(),
                attr(tag, "plainToken"),
                attr(tag, "regexToken"),
            )
        {
            tokens.push((engine, plain, re));
        }
    }
    engines
        .into_iter()
        .map(|(name, domain)| {
            let keys = tokens
                .iter()
                .filter(|(e, _, _)| *e == name)
                .map(|(_, p, r)| (p.clone(), r.clone()))
                .collect();
            let domain_re = Regex::new(&format!("^(.*[./])?{domain}([./].*)?$")).ok();
            SearchEngine {
                name,
                domain_substring: domain,
                keys,
                domain_re,
            }
        })
        .collect()
}

/// Autopsy's engines.
pub fn engines() -> &'static [SearchEngine] {
    static E: OnceLock<Vec<SearchEngine>> = OnceLock::new();
    E.get_or_init(|| parse_mappings(MAPPINGS_XML))
}

/// Autopsy `getValue`: splits `url` on the regex token and keeps the last
/// piece up to the first `&`.
fn split_value(url: &str, regex_token: &str) -> String {
    let v = regex_token.replace("\\?", "?");
    let Ok(re) = Regex::new(&v) else {
        return String::new();
    };
    let mut pieces: Vec<&str> = re.split(url).collect();
    while pieces.last().is_some_and(|p| p.is_empty()) {
        pieces.pop();
    }
    if pieces.len() < 2 {
        return String::new();
    }
    let last = pieces.last().copied().unwrap_or_default();
    if last.contains('&') {
        last.split('&').next().unwrap_or_default().to_owned()
    } else {
        last.to_owned()
    }
}

/// `replaceAll("%(?![0-9a-fA-F]{2})", "%25")`.
fn escape_stray_percent(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        if c == '%' {
            let ok = b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit);
            out.push_str(if ok { "%" } else { "%25" });
        } else {
            out.push(c);
        }
    }
    out
}

/// Java `URLDecoder.decode(s, "UTF-8")` for input whose `%` signs are
/// all followed by two hex digits.
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        match c {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let v = b
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(v) = v {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Every distinct non-empty search query in `url`, over all engines whose
/// pattern matches it (Autopsy keeps them in a set per artifact).
pub fn search_queries(url: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in engines() {
        if e.matches(url) {
            let q = e.query(url);
            if !q.is_empty() && !out.contains(&q) {
                out.push(q);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_load_with_shared_tokens() {
        let es = engines();
        assert_eq!(es.len(), 30);
        let google = es.iter().find(|e| e.name == "Google").unwrap();
        assert_eq!(
            google.keys,
            vec![
                ("?q=".to_owned(), "\\\\?q=".to_owned()),
                ("&q=".to_owned(), "&q=".to_owned())
            ]
        );
        // Both "Wiki" engines carry both Wiki tokens.
        let wikis: Vec<_> = es.iter().filter(|e| e.name == "Wiki").collect();
        assert_eq!(wikis.len(), 2);
        assert!(wikis.iter().all(|w| w.keys.len() == 2));
    }

    #[test]
    fn queries_like_autopsy() {
        assert_eq!(
            search_queries("http://www.google.com/search?hl=en&q=hacking+tools%21&btnG=x"),
            vec!["hacking tools!"]
        );
        assert!(search_queries("http://www.google.com/").is_empty());
        // Trailing empty pieces are dropped by Java's split.
        assert!(search_queries("http://www.google.com/search?q=").is_empty());
        assert_eq!(
            search_queries("https://www.bing.com/search?q=rust+lang&form=QBLH"),
            vec!["rust lang"]
        );
        assert_eq!(
            search_queries("https://duckduckgo.com/?q=a%2Bb%"),
            vec!["a+b%"]
        );
        // The domain pattern is matched against the whole URL.
        assert_eq!(
            search_queries("http://example.com/google/x?q=found"),
            vec!["found"]
        );
        // Unicode is decoded as UTF-8.
        assert_eq!(
            search_queries("https://yandex.ru/search/?text=%D0%BF%D1%80%D0%B8"),
            vec!["при"]
        );
        // Whitespace-only queries are kept (Autopsy checks for "").
        assert_eq!(search_queries("http://google.com/?q=+"), vec![" "]);
        // A regex token that never matches (literal backslash) yields nothing.
        assert!(search_queries("http://baidu.com/s?qw=x").is_empty());
    }

    #[test]
    fn decoding() {
        assert_eq!(decode("100%25"), "100%");
        assert_eq!(escape_stray_percent("%zz%41%"), "%25zz%41%25");
        assert_eq!(decode(&escape_stray_percent("%E2%82")), "\u{fffd}");
    }
}

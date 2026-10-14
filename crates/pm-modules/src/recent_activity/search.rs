//! Web search queries in visited URLs (Autopsy
//! `SearchEngineURLQueryAnalyzer`, engine definitions from its
//! `SEUQAMappings.xml`, embedded verbatim).

use pm_ingest::JobContext;

use super::{NAME, Visit, extract_domain};
use crate::util::Art;

const MAPPINGS: &str = include_str!("../../data/SEUQAMappings.xml");

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Engine {
    pub(crate) name: String,
    pub(crate) domain: String,
    /// `plainToken`s in file order.
    pub(crate) tokens: Vec<String>,
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let start = tag.find(&format!("{name}=\""))? + name.len() + 2;
    let len = tag.get(start..)?.find('"')?;
    Some(tag.get(start..start + len)?.replace("&amp;", "&"))
}

/// Parses the engine definitions.
pub(crate) fn engines() -> Vec<Engine> {
    let mut out: Vec<Engine> = Vec::new();
    for tag in MAPPINGS.split('<').skip(1) {
        if tag.starts_with("SearchEngine ") {
            if let (Some(name), Some(domain)) = (attr(tag, "engine"), attr(tag, "domainSubstring")) {
                out.push(Engine {
                    name,
                    domain,
                    tokens: Vec::new(),
                });
            }
        } else if tag.starts_with("splitToken ")
            && let (Some(e), Some(t)) = (out.last_mut(), attr(tag, "plainToken"))
        {
            e.tokens.push(t);
        }
    }
    out
}

/// URL-decodes like Java's `URLDecoder` (`+` is a space; lone `%` kept).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        match c {
            b'+' => out.push(b' '),
            b'%' => match b.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The search text of `url` for `engine` (`None` if no token matches).
pub(crate) fn query_of(engine: &Engine, url: &str) -> Option<String> {
    let token = engine.tokens.iter().find(|t| url.contains(t.as_str()))?;
    let last = url.rsplit(token.as_str()).next()?;
    let value = last.split('&').next().unwrap_or_default();
    let q = decode(value);
    (!q.trim().is_empty()).then_some(q)
}

pub(crate) fn analyse(ctx: &JobContext, visits: &[Visit]) {
    let engines = engines();
    for v in visits {
        let domain = extract_domain(&v.url);
        if domain.is_empty() {
            continue;
        }
        let Some((engine, query)) = engines
            .iter()
            .filter(|e| domain.contains(&e.domain))
            .find_map(|e| query_of(e, &v.url).map(|q| (e, q)))
        else {
            continue;
        };
        Art::data(ctx, NAME, "TSK_WEB_SEARCH_QUERY", v.file_id)
            .text("TSK_DOMAIN", &domain)
            .text("TSK_TEXT", query)
            .text("TSK_PROG_NAME", &engine.name)
            .time("TSK_DATETIME_ACCESSED", v.time)
            .post(ctx, &["TSK_TEXT", "TSK_DOMAIN", "TSK_DATETIME_ACCESSED"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_and_queries() {
        let es = engines();
        let google = es.iter().find(|e| e.name == "Google").unwrap();
        assert!(google.tokens.contains(&"&q=".to_owned()));
        assert_eq!(
            query_of(google, "http://www.google.com/search?hl=en&q=hacking+tools%21&btnG=x").as_deref(),
            Some("hacking tools!")
        );
        assert_eq!(query_of(google, "http://www.google.com/"), None);
        assert!(es.len() > 20);
        assert_eq!(decode("100%"), "100%");
    }
}

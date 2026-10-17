//! Host and registrable-domain extraction with Autopsy's semantics
//! (`NetworkUtils.extractDomain`, `DomainTokenizer`, Apache commons
//! `DomainValidator`, `java.net.URL` host parsing).
//!
//! The public-suffix list is Autopsy's own copy (`public_suffix_list.dat`,
//! MPL-2.0), walked the way Autopsy's `DomainTokenizer` trie walks it: the
//! registrable domain is the longest matching suffix path plus one label,
//! where every prefix of a listed rule also counts as a suffix (a trie
//! quirk kept for parity), `*` matches any label and `!label` stops the
//! walk.

use std::collections::HashSet;
use std::sync::OnceLock;

const PSL: &str = include_str!("../data/public_suffix_list.dat");

struct Suffixes {
    /// Every rule and every suffix of a rule, lower-cased (`co.uk`, `uk`,
    /// `*.ck`, `!www.ck`, ...): the trie's node paths.
    nodes: HashSet<String>,
    /// Top-level domains of the ICANN section (for the validator check).
    tlds: HashSet<String>,
}

fn suffixes() -> &'static Suffixes {
    static S: OnceLock<Suffixes> = OnceLock::new();
    S.get_or_init(|| {
        let mut nodes = HashSet::new();
        let mut tlds = HashSet::new();
        let mut icann = false;
        for line in PSL.lines() {
            let t = line.trim();
            if t.contains("===BEGIN ICANN DOMAINS===") {
                icann = true;
            } else if t.contains("===END ICANN DOMAINS===") {
                icann = false;
            }
            if t.is_empty() || t.starts_with("//") {
                continue;
            }
            let lower = t.to_lowercase();
            let labels: Vec<&str> = lower.split('.').filter(|l| !l.trim().is_empty()).collect();
            for i in (0..labels.len()).rev() {
                nodes.insert(labels.get(i..).unwrap_or_default().join("."));
            }
            if icann && labels.len() == 1 && lower.is_ascii() {
                tlds.insert(lower.clone());
            }
        }
        Suffixes { nodes, tlds }
    })
}

/// `node` extended by the child label `label` (`""` is the root).
fn child(node: &str, label: &str) -> String {
    if node.is_empty() {
        label.to_owned()
    } else {
        format!("{label}.{node}")
    }
}

/// Autopsy `DomainTokenizer.getDomain`: the registrable part of `host`
/// (unchanged when its last label is not a known suffix).
pub fn registrable_domain(host: &str) -> String {
    if host.trim().is_empty() {
        return String::new();
    }
    let lower = host.to_lowercase();
    let tokens: Vec<&str> = lower.split('.').filter(|t| !t.trim().is_empty()).collect();
    let s = suffixes();
    let size = tokens.len() as isize;
    let mut idx = size - 1;
    let mut node = String::new();
    while idx >= 0 {
        let Some(tok) = tokens.get(idx as usize) else {
            break;
        };
        if s.nodes.contains(&child(&node, &format!("!{tok}"))) {
            break;
        }
        let next = child(&node, tok);
        if s.nodes.contains(&next) {
            node = next;
        } else {
            let wild = child(&node, "*");
            if s.nodes.contains(&wild) {
                node = wild;
            } else {
                break;
            }
        }
        idx -= 1;
    }
    if idx == size - 1 {
        host.to_owned()
    } else {
        let min = idx.max(0) as usize;
        tokens.get(min..).unwrap_or_default().join(".")
    }
}

/// Java `String.replaceFirst(".*:\\/\\/", "")`: removes everything up to
/// the last `://` of the first line that has one.
fn strip_scheme(url: &str) -> String {
    let mut start = 0;
    for line in url.split_inclusive(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']) {
        let body = line.trim_end_matches(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']);
        if let Some(pos) = body.rfind("://") {
            let cut = start + pos + 3;
            let mut out = String::with_capacity(url.len());
            out.push_str(url.get(..start).unwrap_or_default());
            out.push_str(url.get(cut..).unwrap_or_default());
            return out;
        }
        start += line.len();
    }
    url.to_owned()
}

/// Characters that make Autopsy reject a base domain.
fn has_special(s: &str) -> bool {
    // `.*[...].*` with `matches()` cannot cross a line terminator.
    if s.contains(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']) {
        return false;
    }
    s.contains([
        '~', '`', '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '+', '=', '{', '}', '[', ']',
        ';', ':', '?', '<', '>', ',', '/', ' ',
    ])
}

/// Autopsy `NetworkUtils.getBaseDomain`.
fn base_domain(url: &str) -> String {
    let clean = strip_scheme(url);
    let toks = java_split(&clean, '/');
    let host = toks.first().map_or(clean.as_str(), |t| t.as_str());
    let base = registrable_domain(host);
    if has_special(&base) || !base.contains('.') {
        return String::new();
    }
    base
}

/// Java `String.split` on one character: trailing empty strings removed.
fn java_split(s: &str, c: char) -> Vec<String> {
    let mut v: Vec<String> = s.split(c).map(str::to_owned).collect();
    while v.last().is_some_and(String::is_empty) {
        v.pop();
    }
    if s.is_empty() {
        return vec![String::new()];
    }
    v
}

fn is_label(l: &str, first_alpha: bool) -> bool {
    let b = l.as_bytes();
    let (Some(&first), Some(&last)) = (b.first(), b.last()) else {
        return false;
    };
    let first_ok = if first_alpha {
        first.is_ascii_alphabetic()
    } else {
        first.is_ascii_alphanumeric()
    };
    first_ok
        && b.len() <= 63
        && last.is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

/// Apache commons `DomainValidator.getInstance(true).isValid` (local
/// names allowed). Non-ASCII names are rejected (no IDN conversion).
pub fn is_valid_domain(domain: &str) -> bool {
    if domain.len() > 253 || !domain.is_ascii() {
        return false;
    }
    let d = domain.strip_suffix('.').unwrap_or(domain);
    let labels: Vec<&str> = d.split('.').collect();
    if labels.len() >= 2 {
        let (tld, rest) = labels.split_last().unwrap_or((&"", &[]));
        if rest.iter().all(|l| is_label(l, false)) && is_label(tld, true) {
            let t = tld.to_ascii_lowercase();
            return t == "localhost"
                || t == "localdomain"
                || t == "arpa"
                || suffixes().tlds.contains(&t);
        }
    }
    // Local host name (single label).
    !domain.contains('.') && is_label(domain, false)
}

fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
                && p.parse::<u16>().is_ok_and(|v| v <= 255)
        })
}

/// Protocols with a `java.net.URL` handler in the JDK.
const JAVA_PROTOCOLS: &[&str] = &["http", "https", "ftp", "file", "jar", "mailto", "jrt"];

/// The host `java.net.URL(url).getHost()` would return: `None` when the
/// constructor throws (`MalformedURLException`), `Some("")` when the URL
/// has no host.
pub fn java_url_host(url: &str) -> Option<String> {
    let spec = url.trim_matches(|c: char| c <= ' ');
    let spec = if spec.len() >= 4
        && spec
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case("url:"))
    {
        spec.get(4..).unwrap_or_default()
    } else {
        spec
    };
    let colon = spec.find(':')?;
    let scheme = spec.get(..colon)?;
    if spec.get(..colon)?.contains('/') {
        return None;
    }
    let mut chars = scheme.chars();
    let valid_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid_scheme {
        return None;
    }
    let proto = scheme.to_ascii_lowercase();
    if !JAVA_PROTOCOLS.contains(&proto.as_str()) {
        return None;
    }
    if matches!(proto.as_str(), "jar" | "mailto" | "jrt") {
        return Some(String::new());
    }
    let rest = spec.get(colon + 1..)?;
    let rest = rest.split('#').next().unwrap_or_default();
    let rest = rest.split('?').next().unwrap_or_default();
    let Some(after) = rest.strip_prefix("//") else {
        return Some(String::new());
    };
    let authority = after.split('/').next().unwrap_or_default();
    let host = match (authority.find('@'), authority.rfind('@')) {
        (Some(a), Some(b)) if a != b => return Some(String::new()),
        (Some(a), _) => authority.get(a + 1..).unwrap_or_default(),
        _ => authority,
    };
    if host.starts_with('[') {
        // IPv6 literal: Java requires the closing bracket.
        let end = host.find(']')?;
        return Some(host.get(..=end).unwrap_or_default().to_owned());
    }
    if let Some(i) = host.find(':') {
        let port = host.get(i + 1..).unwrap_or_default();
        if !port.is_empty() {
            let digits = port.strip_prefix(['+', '-']).unwrap_or(port);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let v: i64 = port.parse().ok()?;
            if v < -1 || v > i64::from(i32::MAX) {
                return None;
            }
        }
        return Some(host.get(..i).unwrap_or_default().to_owned());
    }
    Some(host.to_owned())
}

/// Autopsy `NetworkUtils.extractDomain`: the registrable domain of a URL
/// (or an IPv4 address), `""` when there is none.
pub fn extract_domain(url: &str) -> String {
    let host = java_url_host(url).filter(|h| !h.trim().is_empty());
    let result = match host {
        Some(h) => base_domain(&h),
        None => base_domain(url),
    };
    if is_valid_domain(&result) || is_ipv4(&result) {
        result
    } else {
        String::new()
    }
}

/// Java `replaceFirst("^\\.+(?!$)", "")`: leading dots removed (a string
/// of dots keeps one).
pub fn strip_leading_dots(s: &str) -> String {
    let t = s.trim_start_matches('.');
    if t.is_empty() && !s.is_empty() {
        ".".to_owned()
    } else {
        t.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrable_domains_follow_the_trie() {
        assert_eq!(registrable_domain("www.google.com"), "google.com");
        assert_eq!(registrable_domain("news.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(registrable_domain("a.b.foo.ck"), "b.foo.ck");
        assert_eq!(registrable_domain("www.ck"), "www.ck");
        assert_eq!(registrable_domain("x.blogspot.com"), "x.blogspot.com");
        assert_eq!(registrable_domain("google.com"), "google.com");
        assert_eq!(registrable_domain("com"), "com");
        // Unknown TLD: returned unchanged (original case).
        assert_eq!(registrable_domain("Host.Unknowntld"), "Host.Unknowntld");
        assert_eq!(registrable_domain(""), "");
    }

    #[test]
    fn extract_domain_like_autopsy() {
        assert_eq!(
            extract_domain("https://www.google.com/search?q=x"),
            "google.com"
        );
        assert_eq!(extract_domain("http://news.bbc.co.uk/x"), "bbc.co.uk");
        assert_eq!(extract_domain("http://192.168.1.1:8080/"), "192.168.1.1");
        assert_eq!(extract_domain("res://ieframe.dll/acr.htm"), "");
        assert_eq!(extract_domain("file:///C:/x.html"), "");
        assert_eq!(extract_domain("localhost"), "");
        assert_eq!(extract_domain("about:blank"), "");
        assert_eq!(extract_domain("www.example.com/path"), "example.com");
        assert_eq!(extract_domain(".example.com"), "example.com");
        assert_eq!(
            extract_domain("http://user@mail.yahoo.co.jp/"),
            "yahoo.co.jp"
        );
        assert_eq!(extract_domain("http://a@b@evil.com/"), "");
        assert_eq!(extract_domain("http://www.EXAMPLE.org:abc/"), "");
        assert_eq!(extract_domain("place:sort=8&maxResults=10"), "");
        assert_eq!(
            extract_domain("http://under_score.example.com/"),
            "example.com"
        );
        assert_eq!(extract_domain("http://bad_label.com/"), "");
        assert_eq!(extract_domain("http://foo.notatld/"), "");
        assert_eq!(extract_domain(""), "");
    }

    #[test]
    fn java_hosts() {
        assert_eq!(
            java_url_host("HTTP://Example.COM:80/x").as_deref(),
            Some("Example.COM")
        );
        assert_eq!(java_url_host("http://h?x=@y").as_deref(), Some("h"));
        assert_eq!(java_url_host("file:///C:/x").as_deref(), Some(""));
        assert_eq!(java_url_host("res://x.dll/"), None);
        assert_eq!(java_url_host("no scheme"), None);
        assert_eq!(java_url_host("http://[::1]:80/").as_deref(), Some("[::1]"));
    }

    #[test]
    fn helpers() {
        assert_eq!(strip_leading_dots("..a.com"), "a.com");
        assert_eq!(strip_leading_dots("..."), ".");
        assert_eq!(strip_leading_dots(""), "");
        assert_eq!(strip_scheme("a://b://c/d"), "c/d");
        assert!(is_valid_domain("example.com"));
        assert!(is_valid_domain("example.com."));
        assert!(!is_valid_domain("-a.com"));
        assert!(is_valid_domain("host.localdomain"));
        assert!(is_ipv4("10.0.0.255"));
        assert!(!is_ipv4("10.0.0.256"));
        assert!(!is_ipv4("01.0.0.1"));
        assert_eq!(java_split("/", '/'), Vec::<String>::new());
        assert_eq!(java_split("", '/'), vec![String::new()]);
    }
}

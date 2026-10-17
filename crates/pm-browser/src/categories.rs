//! Domain categorisation (Autopsy `DomainCategoryRunner` with
//! `DefaultPriorityDomainCategorizer`, `DefaultDomainCategorizer` and the
//! user's `CustomWebCategorizer`).
//!
//! Categorizers are asked in order — custom, priority (search engines),
//! default CSV — and the first answer wins. Each answer carries the host
//! suffix that matched, which the runner uses to report a category only
//! once per suffix.

use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

use regex::Regex;

use crate::domain::{extract_domain, java_url_host};

/// A category for a host suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainCategory {
    pub host_suffix: String,
    pub category: String,
}

/// Autopsy's `default_domain_categories.csv` (gzipped).
const DEFAULT_CSV_GZ: &[u8] = include_bytes!("../data/default_domain_categories.csv.gz");

const SEARCH_ENGINE: &str = "Search Engine";

// https://www.google.com/supported_domains, as listed by Autopsy.
const GOOGLE_DOMAINS: &[&str] = &[
    "google.com",
    "google.ad",
    "google.ae",
    "google.com.af",
    "google.com.ag",
    "google.com.ai",
    "google.al",
    "google.am",
    "google.co.ao",
    "google.com.ar",
    "google.as",
    "google.at",
    "google.com.au",
    "google.az",
    "google.ba",
    "google.com.bd",
    "google.be",
    "google.bf",
    "google.bg",
    "google.com.bh",
    "google.bi",
    "google.bj",
    "google.com.bn",
    "google.com.bo",
    "google.com.br",
    "google.bs",
    "google.bt",
    "google.co.bw",
    "google.by",
    "google.com.bz",
    "google.ca",
    "google.cd",
    "google.cf",
    "google.cg",
    "google.ch",
    "google.ci",
    "google.co.ck",
    "google.cl",
    "google.cm",
    "google.cn",
    "google.com.co",
    "google.co.cr",
    "google.com.cu",
    "google.cv",
    "google.com.cy",
    "google.cz",
    "google.de",
    "google.dj",
    "google.dk",
    "google.dm",
    "google.com.do",
    "google.dz",
    "google.com.ec",
    "google.ee",
    "google.com.eg",
    "google.es",
    "google.com.et",
    "google.fi",
    "google.com.fj",
    "google.fm",
    "google.fr",
    "google.ga",
    "google.ge",
    "google.gg",
    "google.com.gh",
    "google.com.gi",
    "google.gl",
    "google.gm",
    "google.gr",
    "google.com.gt",
    "google.gy",
    "google.com.hk",
    "google.hn",
    "google.hr",
    "google.ht",
    "google.hu",
    "google.co.id",
    "google.ie",
    "google.co.il",
    "google.im",
    "google.co.in",
    "google.iq",
    "google.is",
    "google.it",
    "google.je",
    "google.com.jm",
    "google.jo",
    "google.co.jp",
    "google.co.ke",
    "google.com.kh",
    "google.ki",
    "google.kg",
    "google.co.kr",
    "google.com.kw",
    "google.kz",
    "google.la",
    "google.com.lb",
    "google.li",
    "google.lk",
    "google.co.ls",
    "google.lt",
    "google.lu",
    "google.lv",
    "google.com.ly",
    "google.co.ma",
    "google.md",
    "google.me",
    "google.mg",
    "google.mk",
    "google.ml",
    "google.com.mm",
    "google.mn",
    "google.ms",
    "google.com.mt",
    "google.mu",
    "google.mv",
    "google.mw",
    "google.com.mx",
    "google.com.my",
    "google.co.mz",
    "google.com.na",
    "google.com.ng",
    "google.com.ni",
    "google.ne",
    "google.nl",
    "google.no",
    "google.com.np",
    "google.nr",
    "google.nu",
    "google.co.nz",
    "google.com.om",
    "google.com.pa",
    "google.com.pe",
    "google.com.pg",
    "google.com.ph",
    "google.com.pk",
    "google.pl",
    "google.pn",
    "google.com.pr",
    "google.ps",
    "google.pt",
    "google.com.py",
    "google.com.qa",
    "google.ro",
    "google.ru",
    "google.rw",
    "google.com.sa",
    "google.com.sb",
    "google.sc",
    "google.se",
    "google.com.sg",
    "google.sh",
    "google.si",
    "google.sk",
    "google.com.sl",
    "google.sn",
    "google.so",
    "google.sm",
    "google.sr",
    "google.st",
    "google.com.sv",
    "google.td",
    "google.tg",
    "google.co.th",
    "google.com.tj",
    "google.tl",
    "google.tm",
    "google.tn",
    "google.to",
    "google.com.tr",
    "google.tt",
    "google.com.tw",
    "google.co.tz",
    "google.com.ua",
    "google.co.ug",
    "google.co.uk",
    "google.com.uy",
    "google.co.uz",
    "google.com.vc",
    "google.co.ve",
    "google.vg",
    "google.co.vi",
    "google.com.vn",
    "google.vu",
    "google.ws",
    "google.rs",
    "google.co.za",
    "google.co.zm",
    "google.co.zw",
    "google.cat",
];

// https://www.yahoo.com/everything/world, as listed by Autopsy.
const YAHOO_DOMAINS: &[&str] = &[
    "espanol.yahoo.com",
    "au.yahoo.com",
    "be.yahoo.com",
    "fr-be.yahoo.com",
    "br.yahoo.com",
    "ca.yahoo.com",
    "de.yahoo.com",
    "es.yahoo.com",
    "fr.yahoo.com",
    "in.yahoo.com",
    "id.yahoo.com",
    "ie.yahoo.com",
    "it.yahoo.com",
    "en-maktoob.yahoo.com",
    "malaysia.yahoo.com",
    "nz.yahoo.com",
    "ph.yahoo.com",
    "qc.yahoo.com",
    "ro.yahoo.com",
    "sg.yahoo.com",
    "za.yahoo.com",
    "se.yahoo.com",
    "uk.yahoo.com",
    "yahoo.com",
    "vn.yahoo.com",
    "gr.yahoo.com",
    "maktoob.yahoo.com",
    "hk.yahoo.com",
    "tw.yahoo.com",
    "yahoo.co.jp",
];

const OTHER_SEARCH_ENGINES: &[&str] = &[
    "bing.com",
    "baidu.com",
    "sogou.com",
    "soso.com",
    "duckduckgo.com",
    "swisscows.com",
    "gibiru.com",
    "cutestat.com",
    "youdao.com",
    "biglobe.ne.jp",
    "givewater.com",
    "ekoru.org",
    "ecosia.org",
    "yandex.ru",
    "yandex.com",
];

/// `host` if not blank, else `domain` (Autopsy's `hostToUse`).
fn host_to_use<'a>(domain: &'a str, host: &'a str) -> Option<&'a str> {
    let h = if host.trim().is_empty() { domain } else { host };
    (!h.trim().is_empty()).then_some(h)
}

/// `DefaultPriorityDomainCategorizer`: well-known search engines by exact
/// host (a leading `www.` ignored).
pub fn priority_category(domain: &str, host: &str) -> Option<DomainCategory> {
    let h = host_to_use(domain, host)?.to_lowercase();
    let words: Vec<&str> = h
        .split('.')
        .filter(|w| !w.trim().is_empty())
        .map(str::trim)
        .collect();
    let skip = usize::from(words.first() == Some(&"www"));
    let sanitized = words.get(skip..).unwrap_or_default().join(".");
    let known = GOOGLE_DOMAINS
        .iter()
        .chain(YAHOO_DOMAINS)
        .chain(OTHER_SEARCH_ENGINES)
        .any(|d| *d == sanitized);
    known.then(|| DomainCategory {
        host_suffix: sanitized,
        category: SEARCH_ENGINE.to_owned(),
    })
}

/// A suffix → category table looked up longest suffix first.
#[derive(Debug, Default, Clone)]
pub struct SuffixTable {
    map: HashMap<String, String>,
}

impl SuffixTable {
    /// Adds an entry (suffix lower-cased; blank entries ignored).
    pub fn insert(&mut self, suffix: &str, category: &str) {
        let s = suffix.trim().to_lowercase();
        let c = category.trim();
        if !s.is_empty() && !c.is_empty() {
            self.map.insert(s, c.to_owned());
        }
    }

    /// Entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Longest matching suffix of `host` (whole labels).
    pub fn lookup(&self, domain: &str, host: &str) -> Option<DomainCategory> {
        let h = host_to_use(domain, host)?;
        let tokens: Vec<&str> = h.split('.').collect();
        for i in 0..tokens.len() {
            let s = tokens.get(i..).unwrap_or_default().join(".");
            if let Some(c) = self.map.get(&s).filter(|c| !c.trim().is_empty()) {
                return Some(DomainCategory {
                    host_suffix: s,
                    category: c.clone(),
                });
            }
        }
        None
    }
}

/// `DefaultDomainCategorizer`'s table (Web Email, Disposable Email,
/// Messaging), loaded once.
pub fn default_table() -> &'static SuffixTable {
    static T: OnceLock<SuffixTable> = OnceLock::new();
    T.get_or_init(|| {
        let mut text = String::new();
        let mut t = SuffixTable::default();
        if flate2::read::GzDecoder::new(DEFAULT_CSV_GZ)
            .read_to_string(&mut text)
            .is_err()
        {
            tracing::error!("default domain categories could not be decompressed");
            return t;
        }
        for line in text.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let items: Vec<&str> = line.trim().split(',').collect();
            if let (Some(suffix), Some(cat)) = (items.first(), items.get(1)) {
                t.insert(suffix, cat);
            }
        }
        t
    })
}

/// The categorizer chain: custom table, search engines, default table.
#[derive(Debug, Default, Clone)]
pub struct Categorizer {
    pub custom: SuffixTable,
}

impl Categorizer {
    /// The first categorizer's answer for `domain` / `host`.
    pub fn category(&self, domain: &str, host: &str) -> Option<DomainCategory> {
        if let Some(c) = host_to_use(domain, host)
            .map(str::to_lowercase)
            .and_then(|h| self.custom.lookup("", &h))
        {
            return Some(c);
        }
        priority_category(domain, host).or_else(|| default_table().lookup(domain, host))
    }
}

fn url_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        #[allow(clippy::expect_used, reason = "constant pattern")]
        Regex::new(
            r"^\s*(((?<scheme>[^:/?#]+):?)?//)?(((?<userinfo>[^/?#@]*)@)?(?<host>[^/.?#:]*\.[^/?#:]*)?(:(?<port>[0-9]{1,5}))?/?)?(?<path>([^?#]*)(\?([^#]*))?(#(.*))?)?",
        )
        .expect("valid URL regex")
    })
}

/// `DomainCategoryRunner.getHost`: Java URL host, else the regex host.
pub fn host_of(url: &str) -> Option<String> {
    if let Some(h) = java_url_host(url).filter(|h| !h.trim().is_empty()) {
        return Some(h);
    }
    url_regex()
        .captures(url)
        .and_then(|c| c.name("host"))
        .map(|m| m.as_str().to_owned())
}

/// `DomainCategoryRunner.getDomainAndHost`: lower-cased `(host, domain)`
/// of an artifact's URL and domain, `None` when both are blank.
pub fn host_and_domain(url: Option<&str>, domain: Option<&str>) -> Option<(String, String)> {
    let host = url.filter(|u| !u.trim().is_empty()).and_then(host_of);
    let has_host = host.as_deref().is_some_and(|h| !h.trim().is_empty());
    let has_domain = domain.is_some_and(|d| !d.trim().is_empty());
    let (host, domain) = match (has_host, has_domain) {
        (false, false) => return None,
        (true, false) => {
            let h = host.unwrap_or_default();
            let d = extract_domain(&h);
            (h, d)
        }
        (false, true) => {
            let d = domain.unwrap_or_default().to_owned();
            (d.clone(), d)
        }
        (true, true) => (
            host.unwrap_or_default(),
            domain.unwrap_or_default().to_owned(),
        ),
    };
    Some((host.to_lowercase(), domain.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_search_engines() {
        let c = priority_category("", "www.google.co.uk").unwrap();
        assert_eq!(c.host_suffix, "google.co.uk");
        assert_eq!(c.category, "Search Engine");
        assert!(priority_category("", "mail.google.com").is_none());
        assert_eq!(
            priority_category("bing.com", "").unwrap().host_suffix,
            "bing.com"
        );
        assert!(priority_category("", "").is_none());
    }

    #[test]
    fn default_csv_lookups() {
        let t = default_table();
        assert!(t.len() > 100_000, "{}", t.len());
        let c = t.lookup("", "chat.icq.com").unwrap();
        assert_eq!(c.host_suffix, "icq.com");
        assert_eq!(c.category, "Messaging");
        assert_eq!(t.lookup("", "mail.126.com").unwrap().category, "Web Email");
        assert!(t.lookup("", "example.com").is_none());
    }

    #[test]
    fn chain_prefers_custom() {
        let mut cat = Categorizer::default();
        cat.custom.insert("Example.com", "Work");
        assert_eq!(cat.category("", "a.example.com").unwrap().category, "Work");
        assert_eq!(
            cat.category("", "duckduckgo.com").unwrap().category,
            "Search Engine"
        );
    }

    #[test]
    fn hosts() {
        assert_eq!(
            host_of("https://Mail.Example.com:8080/x").as_deref(),
            Some("Mail.Example.com")
        );
        assert_eq!(
            host_of("mail.example.com/inbox").as_deref(),
            Some("mail.example.com")
        );
        assert_eq!(
            host_of("//cdn.example.com/x").as_deref(),
            Some("cdn.example.com")
        );
        assert_eq!(host_of("nohost"), None);
        assert_eq!(
            host_and_domain(Some("https://WWW.Bing.com/"), None),
            Some(("www.bing.com".into(), "bing.com".into()))
        );
        assert_eq!(
            host_and_domain(None, Some("Example.COM")),
            Some(("example.com".into(), "example.com".into()))
        );
        assert_eq!(host_and_domain(Some(""), Some(" ")), None);
    }
}

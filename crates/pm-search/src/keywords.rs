//! Keyword lists (Autopsy `Keyword`, `KeywordList`, `KeywordSearchList`,
//! `XmlKeywordSearchList`, `EnCaseKeywordSearchList`).
//!
//! * Five locked built-in lists with Autopsy's exact regular expressions;
//!   only "Email Addresses" is searched by default.
//! * Autopsy's `keywords.xml` format (import with or without a BOM, export
//!   without the ingest flags, like Autopsy's "Export").
//! * EnCase keyword exports (UTF-16 text, a tree of folders): every folder
//!   with expressions becomes a list named `parent/child`; GREP
//!   expressions are skipped, as in Autopsy.

use std::fmt::Write as _;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization as _;

/// Autopsy `KeywordSearchList.BOUNDARY_CHARACTERS` (as the regex engine
/// sees it).
pub const BOUNDARY_CHARACTERS: &str =
    "[ \t\r\n\\.\\-\\?\\,\\;\\\\!\\:\\[\\]\\/\\(\\)\\\"\\'\\>\\{\\}]";

/// Built-in list names (Autopsy's, used in settings and `TSK_SET_NAME`).
pub const PHONE_NUMBERS: &str = "Phone Numbers";
pub const IP_ADDRESSES: &str = "IP Addresses";
pub const EMAIL_ADDRESSES: &str = "Email Addresses";
pub const URLS: &str = "URLs";
pub const CREDIT_CARD_NUMBERS: &str = "Credit Card Numbers";

/// Autopsy `PHONE_NUMBER_REGEX`.
pub fn phone_regex() -> String {
    format!(
        "{BOUNDARY_CHARACTERS}(\\([0-9]{{3}}\\)|[0-9]{{3}})([ \\-\\.])[0-9]{{3}}([ \\-\\.])[0-9]{{4}}{BOUNDARY_CHARACTERS}"
    )
}

/// Autopsy `IP_ADDRESS_REGEX`.
pub fn ip_regex() -> String {
    format!(
        "{BOUNDARY_CHARACTERS}(([0-9]|[1-9][0-9]|1[0-9]{{2}}|2[0-4][0-9]|25[0-5])\\.){{3}}(1[0-9]{{2}}|2[0-4][0-9]|25[0-5]|[1-9][0-9]|[0-9]){BOUNDARY_CHARACTERS}"
    )
}

/// Autopsy `EMAIL_ADDRESS_REGEX`.
pub const EMAIL_REGEX: &str = "(\\{?)[a-zA-Z0-9%+_\\-]+(\\.[a-zA-Z0-9%+_\\-]+)*(\\}?)\\@([a-zA-Z0-9]([a-zA-Z0-9\\-]*[a-zA-Z0-9])?\\.)+[a-zA-Z]{2,4}";

/// Autopsy `URL_REGEX`, including its quirks (`(s|S?)`, and the left-over
/// `&amp;` inside the path class).
pub const URL_REGEX: &str = "(((((h|H)(t|T))|(f|F))(t|T)(p|P)(s|S?)\\:\\/\\/)|(w|W){3,3}\\.)[a-zA-Z0-9\\-\\.]+\\.([a-zA-Z]{2,5})(\\:[0-9]+)*(\\/($|[a-zA-Z0-9\\.\\,\\;\\?\\'\\\\+&amp;%\\$#\\=~_\\-]+))*";

/// Autopsy `CCN_REGEX`.
pub const CCN_REGEX: &str = "(%?)(B?)([0-9][ \\-]*?){12,19}(\\^?)";

/// Selector attribute types of the built-in lists.
pub const SEL_PHONE: &str = "TSK_PHONE_NUMBER";
pub const SEL_IP: &str = "TSK_IP_ADDRESS";
pub const SEL_EMAIL: &str = "TSK_EMAIL";
pub const SEL_URL: &str = "TSK_URL";
pub const SEL_CARD: &str = "TSK_CARD_NUMBER";

/// Date format of `keywords.xml` (`yyyy-MM-dd HH:mm:ss`, local time).
const DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// How a keyword matches (Autopsy `KeywordSearch.QueryType`; the ordinal
/// is stored in `TSK_KEYWORD_SEARCH_TYPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchType {
    /// Whole words (`LITERAL`).
    Exact = 0,
    /// Literal anywhere inside a word; the hit is the whole word.
    Substring = 1,
    Regex = 2,
}

impl SearchType {
    /// Autopsy's radio-button label.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Exact => "Exact Match",
            Self::Substring => "Substring Match",
            Self::Regex => "Regular Expression",
        }
    }
}

/// One search term (Autopsy `Keyword`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Keyword {
    /// The term, NFKC-normalised.
    pub term: String,
    /// Literal text (exact or substring) rather than a regular expression.
    pub literal: bool,
    /// Whole-word match (meaningful for literals).
    #[serde(default = "yes")]
    pub whole_word: bool,
    /// Attribute type that receives the hit (`TSK_EMAIL`, …): selects
    /// validators and, for credit cards, account creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}

fn yes() -> bool {
    true
}

impl Keyword {
    /// A keyword of `search_type` (term NFKC-normalised, as Autopsy does).
    pub fn new(term: &str, search_type: SearchType) -> Self {
        Self::from_flags(
            term,
            search_type != SearchType::Regex,
            search_type == SearchType::Exact,
        )
    }

    /// A keyword from Autopsy's `literal`/`whole` flags.
    pub fn from_flags(term: &str, literal: bool, whole_word: bool) -> Self {
        Self {
            term: term.nfkc().collect(),
            literal,
            whole_word,
            selector: None,
        }
    }

    #[must_use]
    pub fn with_selector(mut self, selector: &str) -> Self {
        self.selector = Some(selector.to_owned());
        self
    }

    pub fn search_type(&self) -> SearchType {
        match (self.literal, self.whole_word) {
            (true, true) => SearchType::Exact,
            (true, false) => SearchType::Substring,
            (false, _) => SearchType::Regex,
        }
    }
}

/// A named list of keywords (Autopsy `KeywordList`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeywordList {
    pub name: String,
    /// `yyyy-MM-dd HH:mm:ss` (local time, as Autopsy writes it).
    pub created: String,
    pub modified: String,
    /// Autopsy's "use during ingest" flag (kept for XML round trips; the
    /// ingest job settings decide which lists run).
    #[serde(default = "yes")]
    pub use_for_ingest: bool,
    /// Post an inbox message per hit.
    #[serde(default = "yes")]
    pub ingest_messages: bool,
    /// Built-in lists cannot be edited and are never written to XML.
    #[serde(default)]
    pub locked: bool,
    pub keywords: Vec<Keyword>,
}

impl KeywordList {
    /// A new, editable list created now.
    pub fn new(name: &str, keywords: Vec<Keyword>) -> Self {
        let now = now_string();
        Self {
            name: name.to_owned(),
            created: now.clone(),
            modified: now,
            use_for_ingest: true,
            ingest_messages: true,
            locked: false,
            keywords,
        }
    }

    /// Adds keywords, skipping duplicates (same term, type and selector);
    /// returns how many were added.
    pub fn add_keywords(&mut self, keywords: impl IntoIterator<Item = Keyword>) -> usize {
        let mut n = 0;
        for k in keywords {
            if !self.keywords.contains(&k) {
                self.keywords.push(k);
                n += 1;
            }
        }
        if n > 0 {
            self.modified = now_string();
        }
        n
    }
}

fn now_string() -> String {
    jiff::Zoned::now().strftime(DATE_FORMAT).to_string()
}

fn builtin(name: &str, regex: &str, selector: &str) -> KeywordList {
    KeywordList {
        name: name.to_owned(),
        created: now_string(),
        modified: now_string(),
        use_for_ingest: false,
        ingest_messages: false,
        locked: true,
        keywords: vec![Keyword::from_flags(regex, false, true).with_selector(selector)],
    }
}

/// Autopsy's locked built-in lists, in Autopsy's order.
pub fn builtin_lists() -> Vec<KeywordList> {
    vec![
        builtin(PHONE_NUMBERS, &phone_regex(), SEL_PHONE),
        builtin(IP_ADDRESSES, &ip_regex(), SEL_IP),
        builtin(EMAIL_ADDRESSES, EMAIL_REGEX, SEL_EMAIL),
        builtin(URLS, URL_REGEX, SEL_URL),
        builtin(CREDIT_CARD_NUMBERS, CCN_REGEX, SEL_CARD),
    ]
}

/// Whether a list is searched when the job settings do not say
/// (Autopsy `KeywordSearchJobSettings`: every list except phone numbers,
/// IP addresses, URLs and credit card numbers).
pub fn enabled_by_default(list_name: &str) -> bool {
    !matches!(
        list_name,
        PHONE_NUMBERS | IP_ADDRESSES | URLS | CREDIT_CARD_NUMBERS
    )
}

/// Why keyword lists could not be read.
#[derive(Debug, Error)]
pub enum ListError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid keyword list file: {0}")]
    Format(String),
}

impl ListError {
    fn format(msg: impl std::fmt::Display) -> Self {
        Self::Format(msg.to_string())
    }
}

/// Reads Autopsy `keywords.xml` content (UTF-8, optional BOM). Missing
/// `use_for_ingest`/`ingest_messages` mean `true`, a missing `whole`
/// means `true`; dates must parse (Autopsy rejects the file otherwise).
pub fn parse_xml(data: &[u8]) -> Result<Vec<KeywordList>, ListError> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    let mut reader = quick_xml::Reader::from_reader(data);
    reader.config_mut().trim_text(false);
    let mut lists: Vec<KeywordList> = Vec::new();
    let mut buf = Vec::new();
    let mut cur: Option<KeywordList> = None;
    // Keyword being read: flags and collected text.
    let mut word: Option<(bool, bool, Option<String>, String)> = None;
    let mut saw_root = false;
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(ListError::format)?;
        match ev {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"keyword_lists" => {
                saw_root = true;
            }
            Event::Start(e) if e.local_name().as_ref() == b"keyword_list" => {
                cur = Some(list_from(&e)?);
            }
            Event::Empty(e) if e.local_name().as_ref() == b"keyword_list" => {
                lists.push(list_from(&e)?);
            }
            Event::End(e) if e.local_name().as_ref() == b"keyword_list" => {
                if let Some(l) = cur.take() {
                    lists.push(l);
                }
            }
            Event::Start(e) if e.local_name().as_ref() == b"keyword" && cur.is_some() => {
                let literal = attr(&e, b"literal")?.is_some_and(|v| v == "true");
                let whole = attr(&e, b"whole")?.is_none_or(|v| v.is_empty() || v == "true");
                let selector = attr(&e, b"selector")?.filter(|s| !s.is_empty());
                word = Some((literal, whole, selector, String::new()));
            }
            Event::Empty(e) if e.local_name().as_ref() == b"keyword" => {
                if let Some(l) = cur.as_mut() {
                    let literal = attr(&e, b"literal")?.is_some_and(|v| v == "true");
                    let whole = attr(&e, b"whole")?.is_none_or(|v| v.is_empty() || v == "true");
                    let mut k = Keyword::from_flags("", literal, whole);
                    k.selector = attr(&e, b"selector")?.filter(|s| !s.is_empty());
                    l.keywords.push(k);
                }
            }
            Event::Text(t) => {
                if let Some(w) = word.as_mut() {
                    let s = t.decode().map_err(ListError::format)?;
                    let s = quick_xml::escape::unescape(&s).map_err(ListError::format)?;
                    w.3.push_str(&s);
                }
            }
            Event::GeneralRef(r) => {
                if let Some(w) = word.as_mut() {
                    let name = r.decode().map_err(ListError::format)?;
                    let text = format!("&{name};");
                    let s = quick_xml::escape::unescape(&text).map_err(ListError::format)?;
                    w.3.push_str(&s);
                }
            }
            Event::CData(t) => {
                if let Some(w) = word.as_mut() {
                    w.3.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::End(e) if e.local_name().as_ref() == b"keyword" => {
                if let (Some((literal, whole, selector, text)), Some(l)) =
                    (word.take(), cur.as_mut())
                {
                    let mut k = Keyword::from_flags(&text, literal, whole);
                    k.selector = selector;
                    l.keywords.push(k);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if !saw_root {
        return Err(ListError::format("no <keyword_lists> element"));
    }
    Ok(lists)
}

fn attr(e: &BytesStart<'_>, name: &[u8]) -> Result<Option<String>, ListError> {
    for a in e.attributes().with_checks(false) {
        let a = a.map_err(ListError::format)?;
        if a.key.local_name().as_ref() == name {
            return Ok(Some(
                a.normalized_value(quick_xml::XmlVersion::default())
                    .map_err(ListError::format)?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}

fn list_from(e: &BytesStart<'_>) -> Result<KeywordList, ListError> {
    let name = attr(e, b"name")?.unwrap_or_default();
    let created = attr(e, b"created")?.unwrap_or_default();
    let modified = attr(e, b"modified")?.unwrap_or_default();
    for d in [&created, &modified] {
        jiff::civil::DateTime::strptime(DATE_FORMAT, d)
            .map_err(|err| ListError::format(format!("list {name:?}: bad date {d:?}: {err}")))?;
    }
    let flag = |v: Option<String>| v.is_none_or(|v| v.eq_ignore_ascii_case("true"));
    Ok(KeywordList {
        use_for_ingest: flag(attr(e, b"use_for_ingest")?),
        ingest_messages: flag(attr(e, b"ingest_messages")?),
        name,
        created,
        modified,
        locked: false,
        keywords: Vec::new(),
    })
}

/// Writes lists in Autopsy's `keywords.xml` format. Locked lists are left
/// out; `export` omits the ingest flags (Autopsy's "Export").
pub fn to_xml(lists: &[KeywordList], export: bool) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<keyword_lists>\n",
    );
    for l in lists.iter().filter(|l| !l.locked) {
        let _ = write!(
            s,
            " <keyword_list created=\"{}\" modified=\"{}\" name=\"{}\"",
            esc(&l.created),
            esc(&l.modified),
            esc(&l.name)
        );
        if !export {
            let _ = write!(
                s,
                " use_for_ingest=\"{}\" ingest_messages=\"{}\"",
                l.use_for_ingest, l.ingest_messages
            );
        }
        s.push_str(">\n");
        for k in &l.keywords {
            let _ = write!(
                s,
                "  <keyword literal=\"{}\" whole=\"{}\"",
                k.literal, k.whole_word
            );
            if let Some(sel) = &k.selector {
                let _ = write!(s, " selector=\"{}\"", esc(sel));
            }
            let _ = writeln!(s, ">{}</keyword>", esc(&k.term));
        }
        s.push_str(" </keyword_list>\n");
    }
    s.push_str("</keyword_lists>\n");
    s
}

fn esc(s: &str) -> String {
    quick_xml::escape::escape(s).into_owned()
}

/// Reads lists from a file: Autopsy XML (`.xml`) or an EnCase export
/// (anything else), as Autopsy's import dialog decides.
pub fn import_file(path: &Path) -> Result<Vec<KeywordList>, ListError> {
    const MAX: u64 = 64 * 1024 * 1024;
    let size = std::fs::metadata(path)?.len();
    if size > MAX {
        return Err(ListError::format("file larger than 64 MiB"));
    }
    let data = std::fs::read(path)?;
    let is_xml = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xml"));
    if is_xml {
        parse_xml(&data)
    } else {
        parse_encase(&data)
    }
}

/// One entry of an EnCase keyword export.
#[derive(Debug)]
struct EnCaseEntry {
    name: String,
    value: String,
    folder: bool,
    grep: bool,
    child_count: usize,
    children: Vec<usize>,
}

/// Reads an EnCase keyword export (UTF-16 with BOM, or UTF-16LE): five
/// header lines, then (structure, metadata) line pairs. Folders become
/// lists named by their path below the root; expressions become exact
/// keywords; GREP expressions are skipped; folders without expressions
/// make no list; duplicate names get `(2)`, `(3)`, ….
pub fn parse_encase(data: &[u8]) -> Result<Vec<KeywordList>, ListError> {
    let enc = encoding_rs::Encoding::for_bom(data).map_or(encoding_rs::UTF_16LE, |(e, _)| e);
    let (text, _) = enc.decode_with_bom_removal(data);
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut entries: Vec<EnCaseEntry> = Vec::new();
    let mut it = lines.iter().skip(5);
    while let (Some(structure), Some(meta)) = (it.next(), it.next()) {
        let s: Vec<&str> = structure.split('\t').collect();
        let m: Vec<&str> = meta.split('\t').collect();
        let folder = match m.first().copied().unwrap_or_default() {
            "5" => true,
            "" => false,
            other => {
                return Err(ListError::format(format!(
                    "unknown EnCase entry type {other:?}"
                )));
            }
        };
        let child_count = s
            .get(1)
            .and_then(|c| c.trim().parse::<usize>().ok())
            .ok_or_else(|| ListError::format("bad EnCase child count"))?;
        // Flags start at column 3; GREP (`pg`) is the sixth.
        let grep = m.get(3 + 5).is_some_and(|f| !f.is_empty());
        entries.push(EnCaseEntry {
            name: m.get(1).copied().unwrap_or_default().to_owned(),
            value: m.get(2).copied().unwrap_or_default().to_owned(),
            folder,
            grep,
            child_count,
            children: Vec::new(),
        });
    }
    if entries.is_empty() {
        return Err(ListError::format("no EnCase entries"));
    }
    // The entries are a pre-order walk of the tree; rebuild it from the
    // child counts.
    let mut stack: Vec<usize> = vec![0];
    for i in 1..entries.len() {
        while let Some(&top) = stack.last() {
            let full = entries
                .get(top)
                .is_some_and(|e| e.children.len() >= e.child_count);
            if full {
                stack.pop();
            } else {
                break;
            }
        }
        let Some(&parent) = stack.last() else {
            break;
        };
        if let Some(p) = entries.get_mut(parent) {
            p.children.push(i);
        }
        stack.push(i);
    }
    let mut lists: Vec<KeywordList> = Vec::new();
    make_encase_lists(&entries, 0, "", &mut lists, 0);
    Ok(lists)
}

fn make_encase_lists(
    entries: &[EnCaseEntry],
    idx: usize,
    parent_path: &str,
    lists: &mut Vec<KeywordList>,
    depth: usize,
) {
    let Some(entry) = entries.get(idx) else {
        return;
    };
    if depth > 256 {
        return;
    }
    let mut name = if parent_path.is_empty() {
        entry.name.clone()
    } else {
        format!("{parent_path}/{}", entry.name)
    };
    let mut keywords = Vec::new();
    for &c in &entry.children {
        let Some(child) = entries.get(c) else {
            continue;
        };
        if child.folder {
            make_encase_lists(entries, c, &name, lists, depth + 1);
        } else if !child.grep {
            keywords.push(Keyword::from_flags(&child.value, true, true));
        }
    }
    if lists.iter().any(|l| l.name == name) {
        let mut i = 2;
        while lists.iter().any(|l| l.name == format!("{name}({i})")) {
            i += 1;
        }
        name = format!("{name}({i})");
    }
    if !keywords.is_empty() {
        lists.push(KeywordList::new(&name, keywords));
    }
}

/// Translates a Java regular expression to Rust `regex` syntax where the
/// two differ in ways keyword lists use: `\<`/`\>` are literal angle
/// brackets in Java (word boundaries in Rust), and `\Q…\E` quotes text.
pub fn java_regex(java: &str) -> String {
    let mut out = String::with_capacity(java.len());
    let mut chars = java.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('<') => out.push('<'),
            Some('>') => out.push('>'),
            Some('Q') => {
                let mut quoted = String::new();
                while let Some(q) = chars.next() {
                    if q == '\\' && chars.peek() == Some(&'E') {
                        chars.next();
                        break;
                    }
                    quoted.push(q);
                }
                out.push_str(&regex::escape(&quoted));
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The global keyword lists of a user (Autopsy's `keywords.xml` in the
/// user configuration folder) plus the built-in lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeywordListStore {
    /// Built-in lists first, then the user's lists in file order.
    pub lists: Vec<KeywordList>,
}

impl KeywordListStore {
    /// The built-in lists and the user lists of `path` (missing file: no
    /// user lists).
    pub fn load(path: &Path) -> Result<Self, ListError> {
        let mut lists = builtin_lists();
        match std::fs::read(path) {
            Ok(data) => lists.extend(parse_xml(&data)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Self { lists })
    }

    /// Saves the user lists (atomically: temporary file, then rename).
    pub fn save(&self, path: &Path) -> Result<(), ListError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("xml.tmp");
        std::fs::write(&tmp, to_xml(&self.lists, false))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&KeywordList> {
        self.lists.iter().find(|l| l.name == name)
    }

    /// Adds or replaces an editable list (a locked list's name is refused).
    pub fn put(&mut self, list: KeywordList) -> Result<(), ListError> {
        match self.lists.iter_mut().find(|l| l.name == list.name) {
            Some(l) if l.locked => Err(ListError::format(format!(
                "{:?} is a built-in list",
                list.name
            ))),
            Some(l) => {
                *l = list;
                Ok(())
            }
            None => {
                self.lists.push(list);
                Ok(())
            }
        }
    }

    /// Removes an editable list; returns whether it existed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.lists.len();
        self.lists.retain(|l| l.locked || l.name != name);
        self.lists.len() != before
    }

    /// The lists a job searches by default.
    pub fn default_enabled(&self) -> Vec<KeywordList> {
        self.lists
            .iter()
            .filter(|l| enabled_by_default(&l.name))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Autopsy's `test/input/notablekeywords.xml` (BOM, trailing spaces,
    /// Arabic terms).
    const NOTABLE: &str = "\u{FEFF}<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<keyword_lists>\n <keyword_list created=\"2012-03-23 11:06:17\" modified=\"2012-03-23 11:08:48\" name=\"notable_keywords\" use_for_ingest=\"true\">\n  <keyword literal=\"true\">Jean</keyword>\n  <keyword literal=\"true\">Personalized</keyword>\n  <keyword literal=\"true\">DIRT</keyword>  \n  <keyword literal=\"true\">Osama</keyword>\n  <keyword literal=\"true\">bomb</keyword>\n  <keyword literal=\"true\">hacking</keyword>\n  <keyword literal=\"true\">molotov</keyword>\n  <keyword literal=\"true\">nuclear</keyword>\n  <keyword literal=\"true\">صحافة و إعلام</keyword>\n  <keyword literal=\"true\">مطلوبا</keyword>\n </keyword_list>\n</keyword_lists>\n";

    #[test]
    fn imports_autopsy_notable_keywords() {
        let lists = parse_xml(NOTABLE.as_bytes()).unwrap();
        assert_eq!(lists.len(), 1);
        let l = &lists[0];
        assert_eq!(l.name, "notable_keywords");
        assert!(l.use_for_ingest);
        assert!(l.ingest_messages);
        assert_eq!(l.created, "2012-03-23 11:06:17");
        assert_eq!(l.modified, "2012-03-23 11:08:48");
        let terms: Vec<&str> = l.keywords.iter().map(|k| k.term.as_str()).collect();
        assert_eq!(
            terms,
            [
                "Jean",
                "Personalized",
                "DIRT",
                "Osama",
                "bomb",
                "hacking",
                "molotov",
                "nuclear",
                "صحافة و إعلام",
                "مطلوبا"
            ]
        );
        assert!(
            l.keywords
                .iter()
                .all(|k| k.search_type() == SearchType::Exact)
        );

        // Export → import gives the same lists.
        for export in [false, true] {
            let again = parse_xml(to_xml(&lists, export).as_bytes()).unwrap();
            assert_eq!(again, lists);
        }
        assert!(!to_xml(&lists, true).contains("use_for_ingest"));
        assert!(to_xml(&lists, false).contains("use_for_ingest=\"true\""));
    }

    #[test]
    fn xml_flags_selectors_and_escapes() {
        let xml = r#"<keyword_lists>
            <keyword_list created="2020-01-01 00:00:00" modified="2020-01-02 00:00:00" name="a &amp; b" use_for_ingest="false" ingest_messages="false">
              <keyword literal="false" whole="false" selector="TSK_EMAIL">[a-z]+@x\.com</keyword>
              <keyword literal="true" whole="false">C&lt;3</keyword>
              <keyword literal="true" whole="">blank</keyword>
            </keyword_list></keyword_lists>"#;
        let l = &parse_xml(xml.as_bytes()).unwrap()[0];
        assert_eq!(l.name, "a & b");
        assert!(!l.use_for_ingest && !l.ingest_messages);
        assert_eq!(l.keywords[0].search_type(), SearchType::Regex);
        assert_eq!(l.keywords[0].selector.as_deref(), Some("TSK_EMAIL"));
        assert_eq!(l.keywords[1].term, "C<3");
        assert_eq!(l.keywords[1].search_type(), SearchType::Substring);
        assert_eq!(l.keywords[2].search_type(), SearchType::Exact);
        let round = parse_xml(to_xml(std::slice::from_ref(l), false).as_bytes()).unwrap();
        assert_eq!(&round[0], l);
    }

    #[test]
    fn bad_xml_is_an_error() {
        assert!(parse_xml(b"not xml").is_err());
        assert!(
            parse_xml(b"<keyword_lists><keyword_list name='x' created='yesterday' modified='2020-01-01 00:00:00'/></keyword_lists>")
                .is_err()
        );
        assert!(parse_xml(b"<keyword_lists/>").unwrap().is_empty());
    }

    #[test]
    fn builtins_are_autopsys() {
        let b = builtin_lists();
        let names: Vec<&str> = b.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            [
                PHONE_NUMBERS,
                IP_ADDRESSES,
                EMAIL_ADDRESSES,
                URLS,
                CREDIT_CARD_NUMBERS
            ]
        );
        assert!(b.iter().all(|l| l.locked && !l.use_for_ingest));
        assert!(
            b.iter()
                .all(|l| l.keywords[0].search_type() == SearchType::Regex)
        );
        assert_eq!(
            phone_regex(),
            "[ \t\r\n\\.\\-\\?\\,\\;\\\\!\\:\\[\\]\\/\\(\\)\\\"\\'\\>\\{\\}](\\([0-9]{3}\\)|[0-9]{3})([ \\-\\.])[0-9]{3}([ \\-\\.])[0-9]{4}[ \t\r\n\\.\\-\\?\\,\\;\\\\!\\:\\[\\]\\/\\(\\)\\\"\\'\\>\\{\\}]"
        );
        let defaults: Vec<&str> = b
            .iter()
            .filter(|l| enabled_by_default(&l.name))
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(defaults, [EMAIL_ADDRESSES]);
        assert!(enabled_by_default("my list"));
        // Every built-in compiles after translation.
        for l in &b {
            regex::Regex::new(&java_regex(&l.keywords[0].term)).unwrap();
        }
        // Locked lists are never exported.
        assert_eq!(parse_xml(to_xml(&b, false).as_bytes()).unwrap().len(), 0);
    }

    #[test]
    fn java_regex_translation() {
        assert_eq!(java_regex(r"a\>b\<c"), "a>b<c");
        assert_eq!(java_regex(r"[\>x]"), "[>x]");
        assert_eq!(java_regex(r"\\>"), r"\\>");
        assert_eq!(java_regex(r"\Qa.b*\Ec"), r"a\.b\*c");
        assert_eq!(java_regex(r"\Qunterminated."), r"unterminated\.");
        assert_eq!(java_regex("tail\\"), "tail\\");
        assert_eq!(java_regex(r"\d+\.\w"), r"\d+\.\w");
    }

    #[test]
    fn nfkc_on_construction() {
        assert_eq!(Keyword::new("ﬁle", SearchType::Exact).term, "file");
    }

    fn utf16(s: &str) -> Vec<u8> {
        let mut v = vec![0xFF, 0xFE];
        v.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
        v
    }

    #[test]
    fn encase_tree() {
        // 17 flag columns after type, name, value; GREP is the 6th flag.
        let flags = |grep: bool| {
            let mut f = vec![""; 17];
            if grep {
                f[5] = "x";
            }
            f.join("\t")
        };
        let mut text = String::from("h1\nh2\nh3\nh4\nh5\n");
        let mut entry = |children: usize, ty: &str, name: &str, value: &str, grep: bool| {
            let _ = write!(
                text,
                "0\t{children}\n{ty}\t{name}\t{value}\t{}\n",
                flags(grep)
            );
        };
        entry(3, "5", "Root", "", false); // root folder
        entry(2, "5", "Drugs", "", false);
        entry(0, "", "", "cocaine", false);
        entry(0, "", "", "c[o0]ke", true); // GREP: skipped
        entry(1, "5", "Empty", "", false);
        entry(0, "", "", "g.r.e.p", true); // only a GREP child: no list
        entry(1, "5", "Drugs", "", false); // duplicate name
        entry(0, "", "", "heroin", false);
        let lists = parse_encase(&utf16(&text)).unwrap();
        let summary: Vec<(String, Vec<String>)> = lists
            .iter()
            .map(|l| {
                (
                    l.name.clone(),
                    l.keywords.iter().map(|k| k.term.clone()).collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Root/Drugs".to_owned(), vec!["cocaine".to_owned()]),
                ("Root/Drugs(2)".to_owned(), vec!["heroin".to_owned()]),
            ]
        );
        assert!(lists.iter().all(|l| {
            l.keywords
                .iter()
                .all(|k| k.search_type() == SearchType::Exact)
        }));
        assert!(parse_encase(&utf16("a\nb\n")).is_err());
        assert!(parse_encase(&utf16("1\n2\n3\n4\n5\n0\tx\n9\ta\n")).is_err());
    }

    #[test]
    fn store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("keywords.xml");
        let mut store = KeywordListStore::load(&path).unwrap();
        assert_eq!(store.lists.len(), 5);
        let mut l = KeywordList::new("mine", vec![]);
        assert_eq!(
            l.add_keywords([
                Keyword::new("a", SearchType::Exact),
                Keyword::new("a", SearchType::Exact),
                Keyword::new("a", SearchType::Substring),
            ]),
            2
        );
        store.put(l).unwrap();
        assert!(store.put(KeywordList::new(URLS, vec![])).is_err());
        store.save(&path).unwrap();
        let again = KeywordListStore::load(&path).unwrap();
        assert_eq!(again.get("mine").unwrap().keywords.len(), 2);
        assert_eq!(again.default_enabled().len(), 2);
        let mut again = again;
        assert!(again.remove("mine"));
        assert!(!again.remove(URLS));
        assert_eq!(again.lists.len(), 5);
    }
}

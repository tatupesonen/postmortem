//! Inline keyword matching (Autopsy `InlineSearcher`), compiled once per
//! job instead of per chunk.
//!
//! Every chunk is checked against all keywords in one pass: literal terms
//! through one Aho-Corasick automaton, regular expressions through one
//! `RegexSet`. Only keywords that pass get their hits extracted:
//!
//! * **exact** (literal, whole word): consecutive word tokens equal to the
//!   keyword's tokens; the hit is the text from the first token's start to
//!   the last token's end (Autopsy's bug of not re-examining tokens after a
//!   partial multi-word match is fixed);
//! * **substring** (literal): the keyword inside a "word" of `[A-Za-z0-9_.']`
//!   characters; the hit is that whole word, lower-cased, without leading
//!   or trailing boundary characters (`ell` → `hello`);
//! * **regex**: every non-empty, case-insensitive match, lower-cased.
//!   Phone-number and IP-address hits lose one leading and one trailing
//!   boundary character (and the next search starts one character back),
//!   e-mail hits need eight characters and a known top-level domain, and
//!   credit-card hits must contain a valid card number.
//!
//! Hits are de-duplicated per keyword and text. Each has Autopsy's
//! snippet: 20 characters, `«hit«`, 20 characters.

use std::collections::HashSet;

use aho_corasick::AhoCorasick;
use regex::{Regex, RegexBuilder, RegexSet, RegexSetBuilder};

use crate::ccn::ccn_hit_is_valid;
use crate::keywords::{
    KeywordList, SEL_CARD, SEL_EMAIL, SEL_IP, SEL_PHONE, SearchType, java_regex,
};
use crate::tld::is_valid_tld;
use crate::tokenize::{Word, word_terms, words};

/// Delimiter around the hit in a snippet (Autopsy `SNIPPET_DELIMITER`).
pub const SNIPPET_DELIMITER: char = '\u{AB}';
/// Characters of context on each side of a snippet.
const SNIPPET_CONTEXT: usize = 20;
/// Shortest e-mail hit kept (Autopsy `MIN_EMAIL_ADDR_LENGTH`).
const MIN_EMAIL_LEN: usize = 8;
/// Compiled-size limit per regular expression.
const REGEX_SIZE_LIMIT: usize = 32 * 1024 * 1024;

/// A keyword that cannot be searched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidKeyword {
    pub list: String,
    pub term: String,
    pub error: String,
}

/// One searchable keyword.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Index of the list in the lists given to [`Matcher::new`].
    pub list: usize,
    /// Index of the keyword in its list.
    pub keyword: usize,
    pub list_name: String,
    /// The keyword as configured.
    pub term: String,
    pub search_type: SearchType,
    pub selector: Option<String>,
    kind: Kind,
}

#[derive(Debug, Clone)]
enum Kind {
    /// Lower-cased word tokens (never empty).
    Exact(Vec<String>),
    /// `[\w.']*` + term + `[\w.']*`.
    Substring(Regex),
    /// The regex and whether its source ends with `.*` (no phone/IP
    /// trimming then).
    Regex(Regex, bool),
}

/// One keyword hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Index into [`Matcher::entries`].
    pub entry: usize,
    /// The hit text (lower-cased except for exact matches).
    pub hit: String,
    /// Autopsy's preview: context, `«hit«`, context.
    pub snippet: String,
    /// Byte range of the match in the searched text.
    pub start: usize,
    pub end: usize,
}

/// All enabled keywords, compiled.
#[derive(Debug)]
pub struct Matcher {
    entries: Vec<Entry>,
    /// Literal prefilter over the lower-cased text: pattern → entry.
    literals: Option<AhoCorasick>,
    literal_entries: Vec<usize>,
    /// Regex prefilter: pattern → entry.
    regexes: Option<RegexSet>,
    regex_entries: Vec<usize>,
    has_exact: bool,
}

/// Compiles a keyword regular expression (Java syntax translated,
/// case-insensitive).
pub fn compile_regex(term: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(&java_regex(term))
        .case_insensitive(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
}

fn substring_regex(term: &str) -> Result<Regex, regex::Error> {
    let pattern = format!(
        "[0-9A-Za-z_.']*(?i:{})[0-9A-Za-z_.']*",
        regex::escape(&term.to_lowercase())
    );
    RegexBuilder::new(&pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
}

impl Matcher {
    /// Compiles the keywords of `lists`. Keywords that cannot be used are
    /// returned (and left out): invalid regular expressions, empty terms,
    /// exact terms without words.
    pub fn new(lists: &[KeywordList]) -> (Self, Vec<InvalidKeyword>) {
        let mut entries = Vec::new();
        let mut invalid = Vec::new();
        for (li, list) in lists.iter().enumerate() {
            for (ki, k) in list.keywords.iter().enumerate() {
                let bad = |error: String| InvalidKeyword {
                    list: list.name.clone(),
                    term: k.term.clone(),
                    error,
                };
                if k.term.is_empty() {
                    invalid.push(bad("empty keyword".into()));
                    continue;
                }
                let st = k.search_type();
                let kind = match st {
                    SearchType::Exact => {
                        let tokens = word_terms(&k.term);
                        if tokens.is_empty() {
                            invalid.push(bad("the keyword has no words".into()));
                            continue;
                        }
                        Kind::Exact(tokens)
                    }
                    SearchType::Substring => match substring_regex(&k.term) {
                        Ok(r) => Kind::Substring(r),
                        Err(e) => {
                            invalid.push(bad(e.to_string()));
                            continue;
                        }
                    },
                    SearchType::Regex => match compile_regex(&k.term) {
                        Ok(r) => Kind::Regex(r, k.term.ends_with(".*")),
                        Err(e) => {
                            invalid.push(bad(e.to_string()));
                            continue;
                        }
                    },
                };
                entries.push(Entry {
                    list: li,
                    keyword: ki,
                    list_name: list.name.clone(),
                    term: k.term.clone(),
                    search_type: st,
                    selector: k.selector.clone(),
                    kind,
                });
            }
        }
        let mut lit_patterns = Vec::new();
        let mut literal_entries = Vec::new();
        let mut re_patterns = Vec::new();
        let mut regex_entries = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            match &e.kind {
                Kind::Exact(_) | Kind::Substring(_) => {
                    lit_patterns.push(e.term.to_lowercase());
                    literal_entries.push(i);
                }
                Kind::Regex(r, _) => {
                    re_patterns.push(r.as_str().to_owned());
                    regex_entries.push(i);
                }
            }
        }
        let literals = if lit_patterns.is_empty() {
            None
        } else {
            AhoCorasick::builder()
                .match_kind(aho_corasick::MatchKind::Standard)
                .build(&lit_patterns)
                .map_err(|e| tracing::warn!("keyword automaton: {e}"))
                .ok()
        };
        let regexes = if re_patterns.is_empty() {
            None
        } else {
            RegexSetBuilder::new(&re_patterns)
                .case_insensitive(true)
                .size_limit(REGEX_SIZE_LIMIT.saturating_mul(4))
                .build()
                .map_err(|e| tracing::warn!("keyword regex set: {e}"))
                .ok()
        };
        let has_exact = entries.iter().any(|e| matches!(e.kind, Kind::Exact(_)));
        let m = Self {
            entries,
            literals,
            literal_entries,
            regexes,
            regex_entries,
            has_exact,
        };
        (m, invalid)
    }

    /// A matcher for one ad-hoc keyword.
    pub fn single(term: &str, search_type: SearchType) -> Result<Self, InvalidKeyword> {
        let list = KeywordList::new("", vec![crate::keywords::Keyword::new(term, search_type)]);
        let (m, mut invalid) = Self::new(std::slice::from_ref(&list));
        match invalid.pop() {
            Some(e) => Err(e),
            None => Ok(m),
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Searches `text` for every keyword; `lower` is `text` lower-cased
    /// when the caller has it (chunks), `None` to compute it (names).
    /// Hits are de-duplicated per keyword.
    pub fn search(&self, text: &str, lower: Option<&str>, out: &mut Vec<Hit>) {
        if self.entries.is_empty() || text.is_empty() {
            return;
        }
        let owned;
        let lower = if let Some(l) = lower {
            l
        } else {
            owned = text.to_lowercase();
            &owned
        };
        let mut candidates: Vec<usize> = Vec::new();
        if let Some(ac) = &self.literals {
            let mut seen = vec![false; self.literal_entries.len()];
            for m in ac.find_overlapping_iter(lower) {
                let p = m.pattern().as_usize();
                if let Some(s) = seen.get_mut(p)
                    && !*s
                {
                    *s = true;
                    if let Some(&e) = self.literal_entries.get(p) {
                        candidates.push(e);
                    }
                }
            }
        }
        if let Some(set) = &self.regexes {
            for p in &set.matches(text) {
                if let Some(&e) = self.regex_entries.get(p) {
                    candidates.push(e);
                }
            }
        }
        candidates.sort_unstable();
        let tokens: Vec<Word> = if self.has_exact
            && candidates.iter().any(|&c| {
                self.entries
                    .get(c)
                    .is_some_and(|e| matches!(e.kind, Kind::Exact(_)))
            }) {
            words(text).collect()
        } else {
            Vec::new()
        };
        for c in candidates {
            self.entry_hits(c, text, &tokens, true, out);
        }
    }

    /// Every occurrence of entry `idx` in `text` (not de-duplicated), for
    /// highlighting.
    pub fn occurrences(&self, idx: usize, text: &str) -> Vec<Hit> {
        let tokens: Vec<Word> = match self.entries.get(idx).map(|e| &e.kind) {
            Some(Kind::Exact(_)) => words(text).collect(),
            _ => Vec::new(),
        };
        let mut out = Vec::new();
        self.entry_hits(idx, text, &tokens, false, &mut out);
        out
    }

    fn entry_hits(
        &self,
        idx: usize,
        text: &str,
        tokens: &[Word],
        dedupe: bool,
        out: &mut Vec<Hit>,
    ) {
        let Some(e) = self.entries.get(idx) else {
            return;
        };
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |hit: String, start: usize, end: usize, out: &mut Vec<Hit>| {
            if dedupe && !seen.insert(hit.clone()) {
                return;
            }
            let snippet = snippet(text, start, end, &hit);
            out.push(Hit {
                entry: idx,
                hit,
                snippet,
                start,
                end,
            });
        };
        match &e.kind {
            Kind::Exact(kw) => {
                let n = kw.len();
                if n == 0 || tokens.len() < n {
                    return;
                }
                for i in 0..=tokens.len() - n {
                    let window = tokens.get(i..i + n).unwrap_or_default();
                    if window.iter().zip(kw).all(|(t, k)| t.lower == *k) {
                        let (Some(first), Some(last)) = (window.first(), window.last()) else {
                            continue;
                        };
                        let hit = text
                            .get(first.start..last.end)
                            .unwrap_or_default()
                            .to_owned();
                        push(hit, first.start, last.end, out);
                    }
                }
            }
            Kind::Substring(re) => {
                for m in re.find_iter(text) {
                    if m.as_str().is_empty() {
                        break;
                    }
                    let hit = m.as_str().to_lowercase();
                    let hit = hit.trim_matches(is_boundary_char).to_owned();
                    if hit.is_empty() {
                        continue;
                    }
                    push(hit, m.start(), m.end(), out);
                }
            }
            Kind::Regex(re, wildcard_suffix) => {
                let trim =
                    !wildcard_suffix && matches!(e.selector.as_deref(), Some(SEL_PHONE | SEL_IP));
                let phone = e.selector.as_deref() == Some(SEL_PHONE);
                let mut offset = 0usize;
                while offset <= text.len() {
                    let Some(m) = re.find_at(text, offset) else {
                        break;
                    };
                    if m.as_str().is_empty() {
                        break;
                    }
                    let mut hit = m.as_str().to_lowercase();
                    offset = m.end();
                    if trim {
                        let keep_first = |c: char| c.is_ascii_digit() || (phone && c == '(');
                        if hit.chars().next().is_some_and(|c| !keep_first(c)) {
                            hit = hit.chars().skip(1).collect();
                        }
                        if hit.chars().last().is_some_and(|c| !c.is_ascii_digit()) {
                            hit.pop();
                        }
                        // The boundary character may start the next match;
                        // always make progress.
                        let back = prev_char_boundary(text, offset);
                        let min = next_char_boundary(text, m.start());
                        offset = back.max(min);
                    }
                    let keep = match e.selector.as_deref() {
                        Some(SEL_EMAIL) => {
                            hit.chars().count() >= MIN_EMAIL_LEN
                                && hit.rfind('.').is_some_and(|d| {
                                    is_valid_tld(hit.get(d + 1..).unwrap_or_default())
                                })
                        }
                        Some(SEL_CARD) => ccn_hit_is_valid(&hit),
                        _ => true,
                    };
                    if keep && !hit.is_empty() {
                        push(hit, m.start(), m.end(), out);
                    }
                }
            }
        }
    }
}

/// Autopsy `KeywordSearchList.BOUNDARY_CHARACTERS` as a predicate.
pub fn is_boundary_char(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '\r'
            | '\n'
            | '.'
            | '-'
            | '?'
            | ','
            | ';'
            | '\\'
            | '!'
            | ':'
            | '['
            | ']'
            | '/'
            | '('
            | ')'
            | '"'
            | '\''
            | '>'
            | '{'
            | '}'
    )
}

fn prev_char_boundary(s: &str, i: usize) -> usize {
    let mut j = i.min(s.len()).saturating_sub(1);
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

fn next_char_boundary(s: &str, i: usize) -> usize {
    let mut j = (i + 1).min(s.len());
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

/// Autopsy `KeywordSearchUtil.makeSnippet`: up to 20 characters before
/// the match, `«hit«`, up to 20 characters after it. (Autopsy never shows
/// the text's last character after a hit; that off-by-one is fixed.)
pub fn snippet(text: &str, start: usize, end: usize, hit: &str) -> String {
    let start = start.min(text.len());
    let end = end.clamp(start, text.len());
    let before = text.get(..start).unwrap_or_default();
    let pre_start = before
        .char_indices()
        .rev()
        .nth(SNIPPET_CONTEXT - 1)
        .map_or(0, |(i, _)| i);
    let after = text.get(end..).unwrap_or_default();
    let post_end = after
        .char_indices()
        .nth(SNIPPET_CONTEXT)
        .map_or(after.len(), |(i, _)| i);
    let mut s = String::with_capacity(hit.len() + 2 * SNIPPET_CONTEXT + 4);
    s.push_str(before.get(pre_start..).unwrap_or_default());
    s.push(SNIPPET_DELIMITER);
    s.push_str(hit);
    s.push(SNIPPET_DELIMITER);
    s.push_str(after.get(..post_end).unwrap_or_default());
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keywords::{Keyword, builtin_lists};

    fn list(keywords: Vec<Keyword>) -> KeywordList {
        KeywordList::new("test", keywords)
    }

    fn hits(m: &Matcher, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        m.search(text, None, &mut out);
        out.into_iter().map(|h| h.hit).collect()
    }

    fn builtin(name: &str) -> Matcher {
        let l = builtin_lists()
            .into_iter()
            .find(|l| l.name == name)
            .unwrap();
        let (m, bad) = Matcher::new(&[l]);
        assert!(bad.is_empty(), "{bad:?}");
        m
    }

    #[test]
    fn exact_words() {
        let (m, _) = Matcher::new(&[list(vec![
            Keyword::new("Jean Paul", SearchType::Exact),
            Keyword::new("bomb", SearchType::Exact),
        ])]);
        assert_eq!(
            hits(&m, "Met JEAN paul at the bomb site."),
            ["JEAN paul", "bomb"]
        );
        // Not a whole word.
        assert!(hits(&m, "bombastic jeans").is_empty());
        // Re-examines tokens after a partial match (Autopsy bug fixed).
        assert_eq!(hits(&m, "jean jean paul"), ["jean paul"]);
        // De-duplicated per text.
        assert_eq!(hits(&m, "bomb, bomb, bomb"), ["bomb"]);
        let all = m.occurrences(1, "bomb, bomb");
        assert_eq!(all.len(), 2);
        assert_eq!((all[1].start, all[1].end), (6, 10));
    }

    #[test]
    fn exact_arabic_and_cjk() {
        let (m, _) = Matcher::new(&[list(vec![
            Keyword::new("صحافة و إعلام", SearchType::Exact),
            Keyword::new("文字", SearchType::Exact),
        ])]);
        assert_eq!(
            hits(&m, "نص صحافة و إعلام نص 中文字符"),
            ["صحافة و إعلام", "文字"]
        );
    }

    #[test]
    fn substring_expands_to_word() {
        let (m, _) = Matcher::new(&[list(vec![Keyword::new("ell", SearchType::Substring)])]);
        assert_eq!(
            hits(&m, "Hello, yellow-shell. 'Bell'"),
            ["hello", "yellow", "shell", "bell"]
        );
        let (m, _) = Matcher::new(&[list(vec![Keyword::new("x", SearchType::Substring)])]);
        assert_eq!(hits(&m, "a.x.b 'x'"), ["a.x.b", "x"]);
    }

    #[test]
    fn regex_keywords() {
        let (m, bad) = Matcher::new(&[list(vec![
            Keyword::new("secret[0-9]+", SearchType::Regex),
            Keyword::new("(unclosed", SearchType::Regex),
            Keyword::new("x*", SearchType::Regex),
            Keyword::new("", SearchType::Exact),
            Keyword::new("!!!", SearchType::Exact),
        ])]);
        assert_eq!(bad.len(), 3);
        assert_eq!(hits(&m, "SECRET42 and secret7"), ["secret42", "secret7"]);
        // Empty matches end the search.
        assert!(hits(&m, "abc").is_empty());
    }

    #[test]
    fn phone_numbers() {
        let m = builtin("Phone Numbers");
        assert_eq!(
            hits(&m, "call (555) 123-4567 or 555.765.4321, x"),
            ["(555) 123-4567", "555.765.4321"]
        );
        // Adjacent numbers share a boundary character.
        assert_eq!(
            hits(&m, " 555-123-4567 555-765-4321 "),
            ["555-123-4567", "555-765-4321"]
        );
        // Needs boundary characters on both sides.
        assert!(hits(&m, "a555-123-4567").is_empty());
        assert!(hits(&m, "555-123-45678 ").is_empty());
    }

    #[test]
    fn ip_addresses() {
        let m = builtin("IP Addresses");
        assert_eq!(
            hits(&m, "host 192.168.1.1, gw 10.0.0.254."),
            ["192.168.1.1", "10.0.0.254"]
        );
        assert!(hits(&m, " 256.1.1.1 ").is_empty());
        assert!(hits(&m, " 1.2.3 ").is_empty());
    }

    #[test]
    fn email_addresses() {
        let m = builtin("Email Addresses");
        assert_eq!(
            hits(&m, "Mail John.Doe@Example.COM or a@b.co, x@y.invalidtld"),
            ["john.doe@example.com"]
        );
        // Shorter than eight characters.
        assert!(hits(&m, "a@b.com").is_empty());
        assert_eq!(
            hits(&m, "{jane}@mail.example.org"),
            ["{jane}@mail.example.org"]
        );
    }

    #[test]
    fn urls() {
        let m = builtin("URLs");
        assert_eq!(
            hits(&m, "see https://example.com/a/b?x=1 and www.test.org."),
            ["https://example.com/a/b?x=1", "www.test.org"]
        );
        assert!(hits(&m, "nothing here").is_empty());
    }

    #[test]
    fn credit_cards() {
        let m = builtin("Credit Card Numbers");
        assert_eq!(
            hits(&m, "card 4539747947839518654 ok"),
            ["4539747947839518654"]
        );
        // Luhn failure; Visa Electron allows no shorter prefix. (A Visa
        // number failing Luhn may still have a valid shorter prefix,
        // which Autopsy's shrinking region accepts.)
        assert!(hits(&m, "card 4026000000659186 ok").is_empty());
        // Autopsy's regex: the greedy repetition succeeds after twelve
        // digits before its lazy separator is ever widened, so a spaced
        // number only matches its first twelve digits (Java, Python and
        // Rust agree), which fail Luhn here. Kept for parity.
        assert!(hits(&m, "4111 1111 1111 1111").is_empty());
        assert!(hits(&m, "4111 1111-1111 1111").is_empty());
        // Track 1 data.
        assert_eq!(hits(&m, "%B4111111111111111^DOE"), ["%b4111111111111111^"]);
    }

    #[test]
    fn snippets() {
        let text = "0123456789abcdefghijKLMNOPQRSTUVWXYZhit0123456789abcdefghijKLMNO";
        let s = text.find("hit").unwrap();
        assert_eq!(
            snippet(text, s, s + 3, "hit"),
            "ghijKLMNOPQRSTUVWXYZ\u{AB}hit\u{AB}0123456789abcdefghij"
        );
        assert_eq!(snippet("ab", 0, 2, "ab"), "\u{AB}ab\u{AB}");
        assert_eq!(snippet("x ab", 2, 4, "ab"), "x \u{AB}ab\u{AB}");
        assert_eq!(
            snippet("äöü hit äöü", 7, 10, "hit"),
            "äöü \u{AB}hit\u{AB} äöü"
        );
    }

    #[test]
    fn single_and_invalid() {
        assert!(Matcher::single("(", SearchType::Regex).is_err());
        let m = Matcher::single("Foo", SearchType::Exact).unwrap();
        assert_eq!(m.entries().len(), 1);
        assert_eq!(hits(&m, "a foo b"), ["foo"]);
        let mut out = Vec::new();
        m.search("", None, &mut out);
        assert!(out.is_empty());
    }
}

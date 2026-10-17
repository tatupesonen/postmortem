//! Tokenisation shared by the inline matcher and the indexes, so an
//! exact-match keyword finds the same words during ingest and ad hoc.
//!
//! * Words: Unicode word boundaries (UAX #29, like Lucene's
//!   `StandardTokenizer`: one token per Han ideograph or Hiragana
//!   character, words of letters and digits otherwise), lower-cased, no
//!   stop words.
//! * Trigrams: every window of three characters of the lower-cased text,
//!   for substring and regular-expression candidate lookups.

use std::collections::HashSet;

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use unicode_segmentation::UnicodeSegmentation as _;

/// Name of the word tokenizer in the Tantivy indexes.
pub const WORDS: &str = "pm_words";
/// Name of the trigram tokenizer (content index; unique trigrams, no
/// positions).
pub const TRIGRAMS: &str = "pm_trigrams";
/// Name of the positional trigram tokenizer (name index).
pub const POS_TRIGRAMS: &str = "pm_pos_trigrams";

/// Start and end markers around a name in the name index, so names
/// shorter than three characters have trigrams and prefix/exact queries
/// can anchor.
pub const NAME_START: char = '\u{2}';
pub const NAME_END: char = '\u{3}';

/// A word of `text`: byte range and lower-cased form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub start: usize,
    pub end: usize,
    pub lower: String,
}

/// The words of `text` in order.
pub fn words(text: &str) -> impl Iterator<Item = Word> + '_ {
    text.unicode_word_indices().map(|(start, w)| Word {
        start,
        end: start + w.len(),
        lower: w.to_lowercase(),
    })
}

/// The lower-cased words of `text` (keyword terms).
pub fn word_terms(text: &str) -> Vec<String> {
    words(text).map(|w| w.lower).collect()
}

/// Lower-cases character by character (no context rules such as the
/// Greek final sigma), so a text and any substring of it lower-case
/// consistently. Used for trigrams only.
pub fn lower_chars(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// Whether a trigram is indexed (all-whitespace trigrams are not).
pub fn is_indexed_trigram(t: &str) -> bool {
    !t.chars().all(char::is_whitespace)
}

/// The distinct indexed trigrams of a text lower-cased with
/// [`lower_chars`].
pub fn trigrams(lower: &str) -> Vec<String> {
    let chars: Vec<char> = lower.chars().collect();
    let mut seen: HashSet<[char; 3]> = HashSet::new();
    let mut out = Vec::new();
    for w in chars.windows(3) {
        if let [a, b, c] = *w
            && !(a.is_whitespace() && b.is_whitespace() && c.is_whitespace())
            && seen.insert([a, b, c])
        {
            out.push([a, b, c].iter().collect());
        }
    }
    out
}

/// The trigrams of a query string in order, with their character
/// offsets (lower-cased).
pub fn query_trigrams(lower: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = lower.chars().collect();
    chars
        .windows(3)
        .enumerate()
        .map(|(i, w)| (i, w.iter().collect()))
        .collect()
}

/// Tantivy tokenizer for [`WORDS`].
#[derive(Debug, Clone, Default)]
pub struct WordTokenizer {
    tokens: Vec<Token>,
}

impl Tokenizer for WordTokenizer {
    type TokenStream<'a> = VecStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> VecStream<'a> {
        self.tokens.clear();
        for (position, w) in words(text).enumerate() {
            self.tokens.push(Token {
                offset_from: w.start,
                offset_to: w.end,
                position,
                text: w.lower,
                position_length: 1,
            });
        }
        VecStream::new(&mut self.tokens)
    }
}

/// Tantivy tokenizer for [`TRIGRAMS`]: distinct trigrams, lower-cased.
#[derive(Debug, Clone, Default)]
pub struct TrigramTokenizer {
    tokens: Vec<Token>,
}

impl Tokenizer for TrigramTokenizer {
    type TokenStream<'a> = VecStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> VecStream<'a> {
        self.tokens.clear();
        let lower = lower_chars(text);
        for (position, t) in trigrams(&lower).into_iter().enumerate() {
            self.tokens.push(Token {
                offset_from: 0,
                offset_to: 0,
                position,
                text: t,
                position_length: 1,
            });
        }
        VecStream::new(&mut self.tokens)
    }
}

/// Tantivy tokenizer for [`POS_TRIGRAMS`]: every trigram of
/// `NAME_START + lower(text) + NAME_END`, at its character position.
#[derive(Debug, Clone, Default)]
pub struct PosTrigramTokenizer {
    tokens: Vec<Token>,
}

impl Tokenizer for PosTrigramTokenizer {
    type TokenStream<'a> = VecStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> VecStream<'a> {
        self.tokens.clear();
        let mut chars = Vec::with_capacity(text.len() + 2);
        chars.push(NAME_START);
        chars.extend(text.chars().flat_map(char::to_lowercase));
        chars.push(NAME_END);
        for (position, w) in chars.windows(3).enumerate() {
            self.tokens.push(Token {
                offset_from: 0,
                offset_to: 0,
                position,
                text: w.iter().collect(),
                position_length: 1,
            });
        }
        VecStream::new(&mut self.tokens)
    }
}

/// A token stream over a prepared vector.
#[derive(Debug)]
pub struct VecStream<'a> {
    tokens: &'a mut [Token],
    /// One past the current token (0 before the first `advance`).
    next: usize,
    /// Returned before the first or after the last token.
    fallback: Token,
}

impl<'a> VecStream<'a> {
    fn new(tokens: &'a mut [Token]) -> Self {
        Self {
            tokens,
            next: 0,
            fallback: Token::default(),
        }
    }
}

impl TokenStream for VecStream<'_> {
    fn advance(&mut self) -> bool {
        if self.next < self.tokens.len() {
            self.next += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        self.next
            .checked_sub(1)
            .and_then(|i| self.tokens.get(i))
            .unwrap_or(&self.fallback)
    }

    fn token_mut(&mut self) -> &mut Token {
        match self
            .next
            .checked_sub(1)
            .and_then(|i| self.tokens.get_mut(i))
        {
            Some(t) => t,
            None => &mut self.fallback,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(mut t: impl Tokenizer, s: &str) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        let mut st = t.token_stream(s);
        while st.advance() {
            out.push((st.token().text.clone(), st.token().position));
        }
        out
    }

    #[test]
    fn words_like_lucene() {
        let w: Vec<String> = word_terms("Hello, World! It's 3.14 e-mail 中文字 foo_bar");
        assert_eq!(
            w,
            [
                "hello", "world", "it's", "3.14", "e", "mail", "中", "文", "字", "foo_bar"
            ]
        );
        let first = words("  Ab cd").next().unwrap();
        assert_eq!((first.start, first.end, first.lower.as_str()), (2, 4, "ab"));
        assert!(word_terms("!!! ...").is_empty());
    }

    #[test]
    fn trigram_sets() {
        assert_eq!(trigrams("abcab"), ["abc", "bca", "cab"]);
        assert_eq!(trigrams("ab"), Vec::<String>::new());
        assert_eq!(trigrams("a   b"), ["a  ", "  b"]);
        assert_eq!(
            query_trigrams("abcd"),
            [(0, "abc".to_owned()), (1, "bcd".to_owned())]
        );
    }

    #[test]
    fn tantivy_streams() {
        assert_eq!(
            texts(WordTokenizer::default(), "One two"),
            [("one".to_owned(), 0), ("two".to_owned(), 1)]
        );
        assert_eq!(
            texts(TrigramTokenizer::default(), "ABCa"),
            [("abc".to_owned(), 0), ("bca".to_owned(), 1)]
        );
        assert_eq!(
            texts(PosTrigramTokenizer::default(), "Ab"),
            [("\u{2}ab".to_owned(), 0), ("ab\u{3}".to_owned(), 1)]
        );
        assert!(texts(WordTokenizer::default(), "").is_empty());
    }
}

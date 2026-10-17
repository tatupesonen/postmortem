//! Autopsy's `Chunker`: splits extracted text into ≈ 32 KB chunks with an
//! overlap window, so a hit that straddles a chunk boundary is still found
//! (in the next chunk).
//!
//! Per chunk (sizes are UTF-8 bytes of both the text and its lower-cased
//! form, which must each stay below the limit):
//! 1. **base**: 512-unit blocks while they fit below 30 720 bytes, then
//!    single characters until a whitespace character or 31 734 bytes;
//! 2. **window**: the same up to 32 248 bytes, then to whitespace up to
//!    32 750 bytes;
//! 3. at the end of the text the window belongs to the base; otherwise it
//!    is pushed back and starts the next chunk.
//!
//! Every block is sanitised: NFKC, then each UTF-16 unit that Solr cannot
//! store becomes `^`. Unlike Autopsy, the chunker is push-based (text
//! arrives from a streaming extractor), a block never ends inside a
//! surrogate pair (Rust strings have none), and the overlap window is
//! re-read from the original text rather than its sanitised copy (the
//! same text except where NFKC composes across a block boundary).

use unicode_normalization::UnicodeNormalization;

use crate::{Stop, TextSink};

const MAX_TOTAL_CHUNK_SIZE: usize = 32_760;
const MINIMUM_BASE_CHUNK_SIZE: usize = 30 * 1024;
const MAXIMUM_BASE_CHUNK_SIZE: usize = 31 * 1024;
const WHITE_SPACE_BUFFER_SIZE: usize = 512;
const READ_CHARS_BUFFER_SIZE: usize = 512;
const MAX_CHAR_SIZE_INCREASE_IN_BYTES: usize = 10;
/// Bytes that must be buffered before a chunk is cut while more text may
/// follow. One chunk reads at most about 32 760 sanitised bytes; NFKC can
/// shrink text about threefold (Hangul jamo), so this leaves a wide
/// margin.
const LOOKAHEAD: usize = 1024 * 1024;

/// One chunk of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Sanitised text: base followed by the overlap window.
    pub text: String,
    /// Lower-cased `text` (what Autopsy searches and stores in
    /// `content_str`).
    pub lower: String,
    /// Autopsy's `chunk_size`: base length in UTF-16 units.
    pub base_units: usize,
    /// Base length in bytes of `text`; hits starting at or after it are
    /// in the overlap and are reported by the next chunk.
    pub base_bytes: usize,
    /// Base length in bytes of `lower`.
    pub base_lower_bytes: usize,
}

#[derive(Default)]
struct Sizes {
    bytes: usize,
    lower: usize,
}

/// Push-based chunker; see the module documentation.
///
/// Unread text sits in `buf` from `pos` on. Reads look ahead and only
/// advance `pos` when the text is taken, so Autopsy's push-backs are
/// position resets; the overlap window is re-read from the raw text.
#[derive(Debug, Default)]
pub struct Chunker {
    buf: String,
    pos: usize,
    /// No more input will come.
    finished: bool,
    eof: bool,
    chunks: usize,
}

impl Chunker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Chunks emitted so far.
    pub fn chunks(&self) -> usize {
        self.chunks
    }

    /// Adds text; emits every chunk that can be cut without seeing more.
    /// At least [`LOOKAHEAD`] bytes stay buffered, more than one chunk
    /// can consume, so only [`Chunker::finish`] sees the end.
    pub fn push(
        &mut self,
        text: &str,
        emit: &mut dyn FnMut(Chunk) -> Result<(), Stop>,
    ) -> Result<(), Stop> {
        if self.pos > 0 && self.pos >= self.buf.len() / 2 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.push_str(text);
        while !self.eof && self.buf.len() - self.pos >= LOOKAHEAD {
            let c = self.next_chunk();
            self.chunks += 1;
            emit(c)?;
        }
        Ok(())
    }

    /// Ends the input and emits the remaining chunks. Like Autopsy, empty
    /// input still yields one (empty) chunk.
    pub fn finish(&mut self, emit: &mut dyn FnMut(Chunk) -> Result<(), Stop>) -> Result<(), Stop> {
        self.finished = true;
        while !self.eof {
            let c = self.next_chunk();
            self.chunks += 1;
            emit(c)?;
        }
        Ok(())
    }

    /// Splits a complete text (convenience for tests and small inputs).
    pub fn split(text: &str) -> Vec<Chunk> {
        let mut out = Vec::new();
        let mut c = Self::new();
        let mut emit = |ch: Chunk| {
            out.push(ch);
            Ok(())
        };
        let _ = c.push(text, &mut emit);
        let _ = c.finish(&mut emit);
        out
    }

    fn rest(&self) -> &str {
        self.buf.get(self.pos..).unwrap_or_default()
    }

    /// Nothing left to read (sets `eof` only once the input is finished;
    /// while pushing, [`LOOKAHEAD`] keeps the buffer from running dry).
    fn at_end(&mut self) -> bool {
        if self.pos < self.buf.len() {
            return false;
        }
        if self.finished {
            self.eof = true;
        }
        true
    }

    fn next_chunk(&mut self) -> Chunk {
        let mut sizes = Sizes::default();
        let mut text = String::new();
        let mut lower = String::new();
        self.read_helper(MINIMUM_BASE_CHUNK_SIZE, &mut text, &mut lower, &mut sizes);
        self.read_to_white_space(MAXIMUM_BASE_CHUNK_SIZE, &mut text, &mut lower, &mut sizes);
        let mut base_units = utf16_len(&text);
        let mut base_bytes = text.len();
        let mut base_lower_bytes = lower.len();
        let window_start = self.pos;
        self.read_helper(
            MAX_TOTAL_CHUNK_SIZE - WHITE_SPACE_BUFFER_SIZE,
            &mut text,
            &mut lower,
            &mut sizes,
        );
        self.read_to_white_space(MAX_TOTAL_CHUNK_SIZE, &mut text, &mut lower, &mut sizes);
        if self.eof {
            base_units = utf16_len(&text);
            base_bytes = text.len();
            base_lower_bytes = lower.len();
        } else {
            // The window starts the next chunk.
            self.pos = window_start;
        }
        Chunk {
            text,
            lower,
            base_units,
            base_bytes,
            base_lower_bytes,
        }
    }

    /// The end (in `buf`) of the next block of up to 512 UTF-16 units.
    fn block_end(&self) -> usize {
        let rest = self.rest();
        let mut units = 0;
        for (i, c) in rest.char_indices() {
            units += c.len_utf16();
            if units > READ_CHARS_BUFFER_SIZE {
                return self.pos + i;
            }
        }
        self.buf.len()
    }

    fn read_helper(&mut self, max: usize, seg: &mut String, lower: &mut String, s: &mut Sizes) {
        while s.bytes < max && s.lower < max && !self.eof {
            if self.at_end() {
                return;
            }
            let end = self.block_end();
            let raw = self.buf.get(self.pos..end).unwrap_or_default();
            let clean = sanitize(raw);
            let low = clean.to_lowercase();
            if s.bytes + clean.len() < max && s.lower + low.len() < max {
                seg.push_str(&clean);
                lower.push_str(&low);
                s.bytes += clean.len();
                s.lower += low.len();
                self.pos = end;
            } else {
                return;
            }
        }
    }

    fn read_to_white_space(
        &mut self,
        max: usize,
        seg: &mut String,
        lower: &mut String,
        s: &mut Sizes,
    ) {
        let limit = max - MAX_CHAR_SIZE_INCREASE_IN_BYTES;
        let mut white = false;
        while s.bytes < limit && s.lower < limit && !white && !self.eof {
            if self.at_end() {
                return;
            }
            let Some(c) = self.rest().chars().next() else {
                return;
            };
            // Autopsy counts the raw character's size here, not the
            // sanitised one.
            let raw_size = c.len_utf8();
            let mut b = [0u8; 4];
            let clean = sanitize(c.encode_utf8(&mut b));
            let low = clean.to_lowercase();
            if s.bytes + raw_size < limit && s.lower + low.len() < limit {
                seg.push_str(&clean);
                lower.push_str(&low);
                s.bytes += raw_size;
                s.lower += low.len();
                white = clean.chars().next().is_some_and(java_is_whitespace);
                self.pos += raw_size;
            } else {
                return;
            }
        }
    }
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Autopsy `Chunker.sanitize`: NFKC, then every character Solr cannot
/// store becomes `^`.
pub fn sanitize(s: &str) -> String {
    if s.is_ascii() {
        // NFKC is the identity on ASCII.
        return s
            .chars()
            .map(|c| if is_valid_solr_char(c) { c } else { '^' })
            .collect();
    }
    if unicode_normalization::is_nfkc_quick(s.chars()) == unicode_normalization::IsNormalized::Yes {
        // Already normalised (most text): skip the decomposition.
        return s
            .chars()
            .map(|c| if is_valid_solr_char(c) { c } else { '^' })
            .collect();
    }
    s.nfkc()
        .map(|c| if is_valid_solr_char(c) { c } else { '^' })
        .collect()
}

/// Autopsy `TextUtil.isValidSolrUTF8`, applied per UTF-16 unit:
/// characters outside the BMP are always valid (their surrogates are).
pub fn is_valid_solr_char(c: char) -> bool {
    let v = c as u32;
    if v > 0xFFFF {
        return true;
    }
    (v <= 0xFDD0 || v >= 0xFDEF)
        && (v > 0x1F || v == 0x9 || v == 0xA || v == 0xD)
        && v != 0xFFFF
        && v != 0xFFFE
}

/// Java's `Character.isWhitespace`: Unicode space, line and paragraph
/// separators except the non-breaking ones, plus `\t`–`\r` and
/// `\u001C`–`\u001F`.
pub fn java_is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{D}'
            | '\u{1C}'..='\u{1F}'
            | ' '
            | '\u{1680}'
            | '\u{2000}'..='\u{2006}'
            | '\u{2008}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

/// A [`TextSink`] that chunks the text and hands every chunk to a
/// callback. Call [`ChunkingSink::finish`] after the extraction.
pub struct ChunkingSink<F: FnMut(Chunk) -> Result<(), Stop>> {
    chunker: Chunker,
    emit: F,
}

impl<F: FnMut(Chunk) -> Result<(), Stop>> std::fmt::Debug for ChunkingSink<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkingSink")
            .field("chunker", &self.chunker)
            .finish_non_exhaustive()
    }
}

impl<F: FnMut(Chunk) -> Result<(), Stop>> ChunkingSink<F> {
    pub fn new(emit: F) -> Self {
        Self {
            chunker: Chunker::new(),
            emit,
        }
    }

    /// Emits the remaining chunks; returns the number of chunks emitted.
    pub fn finish(mut self) -> Result<usize, Stop> {
        self.chunker.finish(&mut self.emit)?;
        Ok(self.chunker.chunks())
    }

    /// Chunks emitted so far.
    pub fn chunks(&self) -> usize {
        self.chunker.chunks()
    }
}

impl<F: FnMut(Chunk) -> Result<(), Stop>> TextSink for ChunkingSink<F> {
    fn push(&mut self, text: &str) -> Result<(), Stop> {
        self.chunker.push(text, &mut self.emit)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn bases(chunks: &[Chunk]) -> String {
        chunks
            .iter()
            .map(|c| c.text.get(..c.base_bytes).unwrap())
            .collect()
    }

    #[test]
    fn short_and_empty_text() {
        let c = Chunker::split("Hello World");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "Hello World");
        assert_eq!(c[0].lower, "hello world");
        assert_eq!(c[0].base_units, 11);
        let e = Chunker::split("");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].text, "");
    }

    #[test]
    fn long_text_is_cut_at_whitespace_with_overlap() {
        let text = "word ".repeat(20_000); // 100 000 bytes
        let chunks = Chunker::split(&text);
        assert_eq!(chunks.len(), 4);
        for c in &chunks[..3] {
            assert!(c.text.len() < MAX_TOTAL_CHUNK_SIZE);
            assert!(c.base_bytes >= MINIMUM_BASE_CHUNK_SIZE - READ_CHARS_BUFFER_SIZE);
            assert!(c.base_bytes <= MAXIMUM_BASE_CHUNK_SIZE);
            // Base ends right after a space.
            assert!(c.text[..c.base_bytes].ends_with(' '));
            // The window is the start of the next chunk.
            assert!(c.text.len() > c.base_bytes);
        }
        for w in chunks.windows(2) {
            let window = &w[0].text[w[0].base_bytes..];
            assert!(w[1].text.starts_with(window));
        }
        assert_eq!(bases(&chunks), text);
    }

    #[test]
    fn sanitising() {
        assert_eq!(sanitize("a\u{1}b\u{FDD5}c\u{FFFE}\t\r\n"), "a^b^c^\t\r\n");
        // NFKC: ligature and full-width letters.
        assert_eq!(sanitize("ﬁ Ａ"), "fi A");
        assert!(is_valid_solr_char('\u{FDD0}'));
        assert!(is_valid_solr_char('\u{1FFFF}'));
        assert!(!java_is_whitespace('\u{A0}'));
        assert!(java_is_whitespace('\u{2003}'));
        assert!(!java_is_whitespace('\u{85}'));
    }

    #[test]
    fn lower_case_growth_is_bounded() {
        // 'İ' lower-cases to two characters (3 bytes vs 2).
        let text = "İ".repeat(40_000);
        let chunks = Chunker::split(&text);
        for c in &chunks {
            assert!(c.lower.len() < MAX_TOTAL_CHUNK_SIZE);
            assert!(c.text.len() < MAX_TOTAL_CHUNK_SIZE);
        }
        assert_eq!(bases(&chunks), text);
    }

    #[test]
    fn streaming_equals_whole() {
        let text: String = (0..60_000)
            .map(|i| if i % 7 == 0 { ' ' } else { 'x' })
            .collect();
        let whole = Chunker::split(&text);
        let mut got = Vec::new();
        let mut sink = ChunkingSink::new(|c| {
            got.push(c);
            Ok(())
        });
        for piece in text.as_bytes().chunks(997) {
            sink.push(std::str::from_utf8(piece).unwrap()).unwrap();
        }
        assert_eq!(sink.finish().unwrap(), whole.len());
        assert_eq!(got, whole);
    }

    #[test]
    fn streaming_large_input_equals_whole() {
        // Several lookahead buffers' worth, pushed in uneven pieces.
        let text: String = (0..3_200_000u32)
            .map(|i| match i % 11 {
                0 => ' ',
                5 => 'ä',
                _ => 'x',
            })
            .collect();
        let whole = Chunker::split(&text);
        let mut got = Vec::new();
        let mut sink = ChunkingSink::new(|c| {
            got.push(c);
            Ok(())
        });
        let mut i = 0;
        while i < text.len() {
            let mut j = (i + 40_009).min(text.len());
            while !text.is_char_boundary(j) {
                j -= 1;
            }
            sink.push(&text[i..j]).unwrap();
            i = j;
        }
        assert_eq!(sink.finish().unwrap(), whole.len());
        assert!(whole.len() > 100);
        assert_eq!(got, whole);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn bases_tile_the_sanitised_text(s in "[a-zA-Z0-9 \\n\\t\u{e9}\u{3b1}\u{4e2d}\u{1f600}]{0,80000}") {
            let chunks = Chunker::split(&s);
            prop_assert_eq!(bases(&chunks), sanitize(&s));
            for c in &chunks {
                prop_assert!(c.text.len() < MAX_TOTAL_CHUNK_SIZE);
                prop_assert!(c.lower.len() < MAX_TOTAL_CHUNK_SIZE);
            }
        }
    }
}

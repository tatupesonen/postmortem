//! Magic byte matching (Tika `MagicMatch`, `MagicDetector`, `AndClause`,
//! `OrClause`, `MinShouldMatchClause`).

use std::fmt;
use std::fmt::Write as _;

use regex::bytes::Regex;

/// Size of the window regex magics run on (`MagicDetector` uses 8 KiB).
const REGEX_WINDOW: usize = 8 * 1024;

/// Why a magic could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid magic {0}")]
pub struct MagicError(pub String);

fn err(s: impl Into<String>) -> MagicError {
    MagicError(s.into())
}

/// Java `Integer.parseInt` / `Long.parseLong` with a radix.
fn parse_java_int(s: &str, radix: u32) -> Result<i64, MagicError> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if digits.is_empty() {
        return Err(err(format!("number {s}")));
    }
    let v = i64::from_str_radix(digits, radix).map_err(|_| err(format!("number {s}")))?;
    Ok(if neg { -v } else { v })
}

/// `MagicDetector.decodeString`: Java escapes, then chars → bytes.
fn decode_string(value: &str, kind: &str) -> Result<Vec<u8>, MagicError> {
    if let Some(hex) = value.strip_prefix("0x") {
        let n = (value.len() - 2) / 2;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let pair = hex.get(i * 2..i * 2 + 2).ok_or_else(|| err(value))?;
            out.push(u8::from_str_radix(pair, 16).map_err(|_| err(value))?);
        }
        return Ok(out);
    }
    // Work on UTF-16 code units like Java.
    let chars: Vec<u16> = value.encode_utf16().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut decoded: Vec<u16> = Vec::with_capacity(chars.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c == u16::from(b'\\') {
            let next = at(i + 1).ok_or_else(|| err(value))?;
            if next == u16::from(b'\\') {
                decoded.push(next);
                i += 1;
            } else if next == u16::from(b'x') {
                let s =
                    String::from_utf16_lossy(chars.get(i + 2..i + 4).ok_or_else(|| err(value))?);
                decoded.push(u16::from_str_radix(&s, 16).map_err(|_| err(value))?);
                i += 3;
            } else if next == u16::from(b'r') {
                decoded.push(u16::from(b'\r'));
                i += 1;
            } else if next == u16::from(b'n') {
                decoded.push(u16::from(b'\n'));
                i += 1;
            } else {
                // Up to three octal digits (`Short.decode("0" + digits)`);
                // none decodes to 0 and the next character is kept.
                let mut j = i + 1;
                while j < i + 4
                    && j < chars.len()
                    && at(j).is_some_and(|c| (u16::from(b'0')..=u16::from(b'9')).contains(&c))
                {
                    j += 1;
                }
                let digits = String::from_utf16_lossy(chars.get(i + 1..j).unwrap_or_default());
                let v = if digits.is_empty() {
                    0
                } else {
                    parse_java_int(&digits, 8)?
                };
                // `Short.byteValue()` written as a char: sign-extended.
                decoded.push(i16::from(v as u8 as i8) as u16);
                i = j - 1;
            }
        } else {
            decoded.push(c);
        }
        i += 1;
    }
    Ok(match kind {
        "unicodeLE" => decoded.iter().flat_map(|c| c.to_le_bytes()).collect(),
        "unicodeBE" => decoded.iter().flat_map(|c| c.to_be_bytes()).collect(),
        _ => decoded.iter().map(|&c| c as u8).collect(),
    })
}

/// `MagicDetector.decodeValue`.
fn decode_value(value: &str, kind: &str) -> Result<Vec<u8>, MagicError> {
    let (tmp, radix) = match value.strip_prefix("0x") {
        Some(h) => (h, 16),
        None => (value, 8),
    };
    Ok(match kind {
        "string" | "regex" | "unicodeLE" | "unicodeBE" => decode_string(value, kind)?,
        "stringignorecase" => decode_string(&value.to_lowercase(), kind)?,
        "byte" => tmp.as_bytes().to_vec(),
        "host16" | "little16" => {
            let i = parse_java_int(tmp, radix)? as i32;
            vec![(i & 0xFF) as u8, (i >> 8) as u8]
        }
        "big16" => {
            let i = parse_java_int(tmp, radix)? as i32;
            vec![(i >> 8) as u8, (i & 0xFF) as u8]
        }
        "host32" | "little32" => {
            let i = parse_java_int(tmp, radix)?;
            vec![i as u8, (i >> 8) as u8, (i >> 16) as u8, (i >> 24) as u8]
        }
        "big32" => {
            let i = parse_java_int(tmp, radix)?;
            vec![(i >> 24) as u8, (i >> 16) as u8, (i >> 8) as u8, i as u8]
        }
        _ => return Err(err(format!("type {kind}"))),
    })
}

/// A compiled regex magic.
#[derive(Debug)]
enum Matcher {
    /// Anchored at the start of the window (`lookingAt`), plus an
    /// unanchored twin to find candidate positions in offset ranges.
    Regex {
        anchored: Regex,
        search: Regex,
    },
    /// The two Tika patterns Rust's `regex` cannot express.
    DBase,
    MsOwner,
}

/// One `<match>` (Tika `MagicMatch` + `MagicDetector`).
#[derive(Debug)]
pub(crate) struct MagicMatch {
    desc: String,
    begin: usize,
    end: usize,
    pattern: Vec<u8>,
    mask: Vec<u8>,
    ignore_case: bool,
    regex: Option<Matcher>,
}

impl MagicMatch {
    /// Compiles a match; `desc` is Tika's `MagicMatch.toString()`.
    pub(crate) fn new(
        media_type: &str,
        kind: &str,
        offset: Option<&str>,
        value: Option<&str>,
        mask: Option<&str>,
    ) -> Result<Self, MagicError> {
        let desc = format!(
            "{media_type} {kind} {} {} {}",
            offset.unwrap_or("null"),
            value.unwrap_or("null"),
            mask.unwrap_or("null")
        );
        let (begin, end) = match offset {
            None => (0, 0),
            Some(o) => match o.find(':') {
                None => {
                    let v = parse_java_int(o, 10)?;
                    (v, v)
                }
                Some(c) => (
                    parse_java_int(&o[..c], 10)?,
                    parse_java_int(&o[c + 1..], 10)?,
                ),
            },
        };
        if begin < 0 || end < begin {
            return Err(err(desc));
        }
        let value = value.ok_or_else(|| err(desc.clone()))?;
        let pat = decode_value(value, kind)?;
        let msk = mask.map(|m| decode_value(m, kind)).transpose()?;
        let len = pat.len().max(msk.as_ref().map_or(0, Vec::len));
        let mask_bytes: Vec<u8> = (0..len)
            .map(|i| msk.as_ref().and_then(|m| m.get(i)).copied().unwrap_or(0xFF))
            .collect();
        let pattern: Vec<u8> = mask_bytes
            .iter()
            .enumerate()
            .map(|(i, m)| pat.get(i).map_or(0, |p| p & m))
            .collect();
        let ignore_case = kind == "stringignorecase";
        let regex = if kind == "regex" {
            let src = String::from_utf8_lossy(&pattern).into_owned();
            Some(compile_java_regex(&src)?)
        } else {
            None
        };
        Ok(Self {
            desc,
            begin: begin as usize,
            end: end as usize,
            pattern,
            mask: mask_bytes,
            ignore_case,
            regex,
        })
    }

    /// `MagicDetector.getLength()` (the pattern length, also for regexes).
    pub(crate) fn size(&self) -> usize {
        self.pattern.len()
    }

    /// `MagicDetector.detect` on the magic prefix.
    pub(crate) fn eval(&self, data: &[u8]) -> bool {
        if data.len() < self.begin {
            return false;
        }
        let range = self.end - self.begin;
        let length = if self.regex.is_some() {
            REGEX_WINDOW
        } else {
            self.pattern.len()
        };
        let avail = data.get(self.begin..).unwrap_or_default();
        let buf_len = length + range;
        if let Some(m) = &self.regex {
            let mut buf = vec![0u8; buf_len];
            for (d, s) in buf.iter_mut().zip(avail) {
                *d = *s;
            }
            return eval_regex(m, &buf, range);
        }
        if avail.len() < length {
            return false;
        }
        let at = |k: usize| avail.get(k).copied().unwrap_or(0);
        (0..=range).any(|i| {
            self.mask
                .iter()
                .zip(&self.pattern)
                .enumerate()
                .all(|(j, (m, p))| {
                    let mut b = at(i + j) & m;
                    if self.ignore_case && b < 0x80 {
                        b = b.to_ascii_lowercase();
                    }
                    b == *p
                })
        })
    }
}

impl fmt::Display for MagicMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.desc)
    }
}

fn eval_regex(m: &Matcher, buf: &[u8], range: usize) -> bool {
    match m {
        Matcher::DBase => (0..=range).any(|i| dbase(buf.get(i..).unwrap_or_default())),
        Matcher::MsOwner => (0..=range).any(|i| ms_owner(buf.get(i..).unwrap_or_default())),
        Matcher::Regex { anchored, search } => {
            if range == 0 {
                return anchored.is_match(buf.get(..REGEX_WINDOW).unwrap_or(buf));
            }
            // Find candidate starts with the unanchored twin, then check
            // the window of each (`region(i, 8192 + i).lookingAt()`).
            let mut from = 0;
            while from <= range {
                let Some(hit) = search.find_at(buf, from) else {
                    return false;
                };
                let s = hit.start();
                if s > range {
                    return false;
                }
                let win = buf.get(s..s + REGEX_WINDOW).unwrap_or_default();
                if anchored.is_match(win) {
                    return true;
                }
                from = s + 1;
            }
            false
        }
    }
}

/// `(?s)^[\x02\x03\x30\x31\x32\x43\x63\x83\x8B\xCB\xF5\xE5\xFB].[\x01-\x0C][\x01-\x1F].{4}(?:.[^\x00]|[\x41-\xFF].)(?:[^\x00\x01].|.[^\x00]).{31}(?<=[\x00][^\x00]{0,10})[A-Z@+]`
fn dbase(b: &[u8]) -> bool {
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    if !matches!(
        at(0),
        0x02 | 0x03 | 0x30 | 0x31 | 0x32 | 0x43 | 0x63 | 0x83 | 0x8B | 0xCB | 0xF5 | 0xE5 | 0xFB
    ) {
        return false;
    }
    if !(0x01..=0x0C).contains(&at(2)) || !(0x01..=0x1F).contains(&at(3)) {
        return false;
    }
    if !(at(9) != 0 || at(8) >= 0x41) {
        return false;
    }
    if matches!(at(10), 0 | 1) && at(11) == 0 {
        return false;
    }
    // Look-behind at 43: a NUL followed by 0..=10 non-NUL bytes.
    let lb = (0..=10usize).any(|k| at(42 - k) == 0 && (43 - k..43).all(|p| at(p) != 0));
    lb && matches!(at(43), b'A'..=b'Z' | b'@' | b'+')
}

/// `(?s)^([\x05-\x0F])[\x00\x20-\x7E]{10}.{43}\1\x00`
fn ms_owner(b: &[u8]) -> bool {
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    let first = at(0);
    (0x05..=0x0F).contains(&first)
        && (1..=10).all(|i| matches!(at(i), 0x00 | 0x20..=0x7E))
        && at(54) == first
        && at(55) == 0
        && b.len() >= 56
}

/// Translates a Java regex (run on ISO-8859-1 text) into a Rust byte
/// regex.
fn compile_java_regex(src: &str) -> Result<Matcher, MagicError> {
    if src.starts_with("(?s)^[\\x02\\x03\\x30") && src.contains("(?<=") {
        return Ok(Matcher::DBase);
    }
    if src.starts_with("(?s)^([\\x05-\\x0F])") && src.contains("\\1") {
        return Ok(Matcher::MsOwner);
    }
    let body = translate(src)?;
    let anchored =
        Regex::new(&format!("(?-u)\\A(?:{body})")).map_err(|e| err(format!("{src}: {e}")))?;
    let search = Regex::new(&format!("(?-u)(?:{body})")).map_err(|e| err(format!("{src}: {e}")))?;
    Ok(Matcher::Regex { anchored, search })
}

/// A literal (Latin-1) character as a byte-regex atom.
fn lit(c: char, out: &mut String) {
    let v = c as u32;
    if v > 0xFF {
        // Cannot occur in ISO-8859-1 text.
        out.push_str("[^\\x00-\\xFF]");
    } else if c.is_ascii_alphanumeric() || c == ' ' {
        out.push(c);
    } else {
        let _ = write!(out, "\\x{v:02X}");
    }
}

fn translate(src: &str) -> Result<String, MagicError> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    // DOTALL state per group nesting level.
    let mut dotall = vec![false];
    let mut i = 0;
    let mut in_class = 0usize;
    while let Some(&c) = chars.get(i) {
        match c {
            '\\' => {
                let n = *chars.get(i + 1).ok_or_else(|| err(src))?;
                i += 1;
                match n {
                    'x' => {
                        let h: String = chars
                            .get(i + 1..i + 3)
                            .ok_or_else(|| err(src))?
                            .iter()
                            .collect();
                        let v = u32::from_str_radix(&h, 16).map_err(|_| err(src))?;
                        let _ = write!(out, "\\x{v:02X}");
                        i += 2;
                    }
                    'u' => {
                        let h: String = chars
                            .get(i + 1..i + 5)
                            .ok_or_else(|| err(src))?
                            .iter()
                            .collect();
                        let v = u32::from_str_radix(&h, 16).map_err(|_| err(src))?;
                        lit(char::from_u32(v).ok_or_else(|| err(src))?, &mut out);
                        i += 4;
                    }
                    'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                        out.push('\\');
                        out.push(n);
                    }
                    'b' | 'B' if in_class == 0 => {
                        out.push('\\');
                        out.push(n);
                    }
                    't' => out.push_str("\\t"),
                    'n' => out.push_str("\\n"),
                    'r' => out.push_str("\\r"),
                    'f' => out.push_str("\\x0C"),
                    'a' => out.push_str("\\x07"),
                    'e' => out.push_str("\\x1B"),
                    '0'..='9' => return Err(err(format!("back-reference or octal in {src}"))),
                    c if c.is_ascii_alphabetic() => {
                        return Err(err(format!("escape \\{c} in {src}")));
                    }
                    c => lit(c, &mut out),
                }
            }
            '[' => {
                in_class += 1;
                out.push('[');
                if chars.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
            }
            ']' if in_class > 0 => {
                in_class -= 1;
                out.push(']');
            }
            '-' | '^' if in_class > 0 => out.push(c),
            '.' if in_class == 0 => {
                if *dotall.last().unwrap_or(&false) {
                    out.push_str("(?s:.)");
                } else {
                    // Java's `.` also excludes CR and NEL.
                    out.push_str("[^\\n\\r\\x85]");
                }
            }
            '(' if in_class == 0 => {
                let rest: String = chars
                    .get(i + 1..(i + 4).min(chars.len()))
                    .unwrap_or_default()
                    .iter()
                    .collect();
                if rest.starts_with("?<=")
                    || rest.starts_with("?<!")
                    || rest.starts_with("?=")
                    || rest.starts_with("?!")
                {
                    return Err(err(format!("look-around in {src}")));
                }
                if rest.starts_with("?s)") {
                    *dotall.last_mut().unwrap_or(&mut false) = true;
                    i += 4;
                    continue;
                }
                if rest.starts_with("?:") {
                    out.push_str("(?:");
                    i += 2;
                } else {
                    out.push('(');
                }
                let cur = *dotall.last().unwrap_or(&false);
                dotall.push(cur);
            }
            ')' if in_class == 0 => {
                if dotall.len() > 1 {
                    dotall.pop();
                }
                out.push(')');
            }
            '^' | '$' | '|' | '*' | '+' | '?' | '{' | '}' if in_class == 0 => out.push(c),
            ',' if in_class == 0 => out.push(','),
            c => lit(c, &mut out),
        }
        i += 1;
    }
    Ok(out)
}

/// A magic clause tree.
#[derive(Debug)]
pub(crate) enum Clause {
    Match(Box<MagicMatch>),
    And(Vec<Clause>),
    Or(Vec<Clause>),
    MinShould(usize, Vec<Clause>),
}

impl Clause {
    pub(crate) fn eval(&self, data: &[u8]) -> bool {
        match self {
            Self::Match(m) => m.eval(data),
            Self::And(c) => c.iter().all(|c| c.eval(data)),
            Self::Or(c) => c.iter().any(|c| c.eval(data)),
            Self::MinShould(min, c) => {
                let mut n = 0;
                for c in c {
                    if c.eval(data) {
                        n += 1;
                        if n >= *min {
                            return true;
                        }
                    }
                }
                false
            }
        }
    }

    /// Tika's `size()`: sum for AND, maximum for OR and min-should-match.
    pub(crate) fn size(&self) -> usize {
        match self {
            Self::Match(m) => m.size(),
            Self::And(c) => c.iter().map(Self::size).sum(),
            Self::Or(c) | Self::MinShould(_, c) => c.iter().map(Self::size).max().unwrap_or(0),
        }
    }
}

impl fmt::Display for Clause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |f: &mut fmt::Formatter<'_>, c: &[Clause]| -> fmt::Result {
            f.write_str("[")?;
            for (i, c) in c.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{c}")?;
            }
            f.write_str("]")
        };
        match self {
            Self::Match(m) => write!(f, "{m}"),
            Self::And(c) => {
                f.write_str("and")?;
                list(f, c)
            }
            Self::Or(c) => {
                f.write_str("or")?;
                list(f, c)
            }
            Self::MinShould(n, c) => {
                write!(f, "minShouldMatch (min: {n}) ")?;
                list(f, c)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(kind: &str, offset: &str, value: &str, mask: Option<&str>) -> MagicMatch {
        MagicMatch::new("x/y", kind, Some(offset), Some(value), mask).unwrap()
    }

    #[test]
    fn decoding() {
        assert_eq!(
            decode_string("a\\x41\\\\\\r\\n\\012", "string").unwrap(),
            b"aA\\\r\n\n"
        );
        // A backslash before another character writes NUL and keeps it.
        assert_eq!(decode_string("\\t", "string").unwrap(), b"\0t");
        assert_eq!(decode_string("0x4142", "string").unwrap(), b"AB");
        assert_eq!(decode_value("0x1234", "big16").unwrap(), [0x12, 0x34]);
        assert_eq!(decode_value("0x1234", "little16").unwrap(), [0x34, 0x12]);
        // Octal by default.
        assert_eq!(decode_value("10", "big16").unwrap(), [0, 8]);
        assert_eq!(
            decode_value("0xcafebabe", "big32").unwrap(),
            [0xCA, 0xFE, 0xBA, 0xBE]
        );
        assert_eq!(decode_value("12", "byte").unwrap(), b"12");
        assert_eq!(decode_value("AB", "unicodeLE").unwrap(), [b'A', 0, b'B', 0]);
        assert_eq!(decode_value("ABc", "stringignorecase").unwrap(), b"abc");
    }

    #[test]
    fn matching() {
        let pdf = m("string", "0", "%PDF-", None);
        assert!(pdf.eval(b"%PDF-1.4"));
        assert!(!pdf.eval(b"%PDF"));
        let ranged = m("string", "2:4", "AB", None);
        assert!(ranged.eval(b"xxxxAB"));
        assert!(!ranged.eval(b"xxxxxAB"));
        // Zero padding past the end for later positions.
        let z = m("string", "0:2", "A\\0", None);
        assert!(z.eval(b"xxA"));
        let masked = m("big16", "0", "0xFF00", Some("0xFF00"));
        assert!(masked.eval(&[0xFF, 0x12]));
        let ic = m("stringignorecase", "0", "<HTML", None);
        assert!(ic.eval(b"<hTmL>"));
        assert_eq!(ic.size(), 5);
    }

    #[test]
    fn regexes() {
        let r = m("regex", "0", "BZh[1-9]", None);
        assert!(r.eval(b"BZh91AY"));
        assert!(!r.eval(b"BZh0"));
        let dot = m("regex", "0", "a.b", None);
        assert!(!dot.eval(b"a\rb"));
        assert!(dot.eval(b"axb"));
        let ai = m(
            "regex",
            "0:8192",
            "[\\r\\n]%AI5_FileFormat [1-4][\\r\\n]",
            None,
        );
        let mut d = vec![b'x'; 5000];
        d.extend_from_slice(b"\n%AI5_FileFormat 3\n");
        assert!(ai.eval(&d));
        assert!(!ai.eval(&d[..5000]));
        // Windows are zero padded.
        let nul = m("regex", "0", "ab\\\\x00", None);
        assert!(nul.eval(b"ab"));
    }

    #[test]
    fn specials() {
        let mut owner = vec![0u8; 162];
        owner[0] = 0x07;
        owner[1..8].copy_from_slice(b"Someone");
        owner[54] = 0x07;
        assert!(ms_owner(&owner));
        owner[55] = 1;
        assert!(!ms_owner(&owner));
        let mut dbf = vec![0u8; 64];
        dbf[0] = 0x03;
        dbf[2] = 5;
        dbf[3] = 7;
        dbf[9] = 1;
        dbf[10] = 0x41;
        dbf[32..43].copy_from_slice(b"\0NAME\0\0\0\0\0\0");
        dbf[42] = 0;
        dbf[43] = b'C';
        assert!(dbase(&dbf));
    }
}

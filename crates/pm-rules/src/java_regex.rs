//! Java `java.util.regex.Pattern` syntax to Rust `regex` syntax, for the
//! patterns Autopsy's files sets use: escaped punctuation and spaces are
//! literals, `\Q…\E` quotes, and a back-reference to a group that does
//! not exist (Java compiles it; it never matches) becomes an impossible
//! assertion. Constructs Rust cannot express (a back-reference to an
//! existing group, possessive quantifiers, atomic groups, look-around)
//! fail translation.

use std::fmt::Write as _;

/// An assertion that never holds.
const NEVER: &str = r"(?:\b\B)";

/// Compiles a Java pattern (`Pattern.compile`); `None` when it is invalid
/// or cannot be expressed in Rust.
pub fn compile(src: &str) -> Option<regex::Regex> {
    regex::Regex::new(&translate(src)?).ok()
}

/// Translates a Java pattern; `None` when it cannot be expressed.
pub fn translate(src: &str) -> Option<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() + 8);
    let mut i = 0;
    let mut groups = 0usize;
    let mut in_class = false;
    while let Some(&c) = chars.get(i) {
        match c {
            '\\' => {
                let next = *chars.get(i + 1)?;
                i += 2;
                match next {
                    'Q' => {
                        // Literal up to \E (or the end).
                        while let Some(&q) = chars.get(i) {
                            if q == '\\' && chars.get(i + 1) == Some(&'E') {
                                i += 2;
                                break;
                            }
                            push_literal(&mut out, q);
                            i += 1;
                        }
                    }
                    '1'..='9' if !in_class => {
                        // Java's `ref()`: more digits only while the group
                        // number stays within the groups seen so far.
                        let mut num = next.to_digit(10).unwrap_or(0) as usize;
                        while let Some(d) = chars.get(i).and_then(|d| d.to_digit(10)) {
                            let n = num * 10 + d as usize;
                            if n > groups {
                                break;
                            }
                            num = n;
                            i += 1;
                        }
                        if num <= groups {
                            return None;
                        }
                        out.push_str(NEVER);
                    }
                    '0' => {
                        // Octal: \0n, \0nn, \0mnn (m ≤ 3).
                        let mut v = 0u32;
                        let mut n = 0;
                        while n < 3 {
                            let Some(d) = chars.get(i).and_then(|d| d.to_digit(8)) else {
                                break;
                            };
                            if n == 2 && v > 0o37 {
                                break;
                            }
                            v = v * 8 + d;
                            i += 1;
                            n += 1;
                        }
                        if n == 0 {
                            return None;
                        }
                        let _ = write!(out, "\\x{{{v:X}}}");
                    }
                    c if c.is_ascii_alphanumeric() => {
                        // Character classes and escapes shared by both
                        // dialects (\d \s \w \b \t \n \x.. \u.. \p{..}).
                        match c {
                            'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'b' | 'B' | 't' | 'n' | 'r'
                            | 'f' | 'a' | 'e' | 'x' | 'p' | 'P' | 'A' | 'z' => {
                                out.push('\\');
                                out.push(c);
                            }
                            'u' => {
                                let hex: String = chars.get(i..i + 4)?.iter().collect();
                                u32::from_str_radix(&hex, 16).ok()?;
                                let _ = write!(out, "\\x{{{hex}}}");
                                i += 4;
                            }
                            'Z' => out.push_str(r"(?:\n?\z)"),
                            _ => return None,
                        }
                    }
                    other => push_literal(&mut out, other),
                }
                continue;
            }
            '[' if !in_class => {
                in_class = true;
                out.push('[');
                // A leading ']' or '^]' is literal in both dialects.
                if chars.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if chars.get(i + 1) == Some(&']') {
                    out.push_str(r"\]");
                    i += 1;
                }
            }
            '[' => out.push_str(r"\["),
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            '&' if in_class && chars.get(i + 1) == Some(&'&') => return None,
            '(' if !in_class => {
                match chars.get(i + 1) {
                    Some('?') => match chars.get(i + 2) {
                        Some(':' | 'i' | 'm' | 's' | 'x' | 'u' | '-') => {}
                        Some('<') if !matches!(chars.get(i + 3), Some('=' | '!')) => {
                            groups += 1;
                        }
                        _ => return None,
                    },
                    _ => groups += 1,
                }
                out.push('(');
            }
            '+' | '*' | '?' | '}' if !in_class => {
                out.push(c);
                // Possessive quantifiers have no Rust equivalent.
                if chars.get(i + 1) == Some(&'+') {
                    return None;
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    Some(out)
}

fn push_literal(out: &mut String, c: char) {
    let mut b = [0u8; 4];
    out.push_str(&regex::escape(c.encode_utf8(&mut b)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rx(s: &str) -> regex::Regex {
        regex::Regex::new(&translate(s).unwrap()).unwrap()
    }

    #[test]
    fn autopsy_patterns() {
        let r = rx(r"/Users/.*\/AppData/.*\/mRemoteNG/.*\.*\10");
        assert!(!r.is_match("/Users/bob/AppData/x/mRemoteNG/y.10//"));
        let r = rx(r" /Users/.*\ /opt/kaseya/.*\ ");
        assert!(r.is_match(" /Users/a /opt/kaseya/b //"));
        let r = rx(r"/Users/.*\/AppData/Local/Devolutions/RemoteDesktopManager[GUID]");
        assert!(r.is_match("/Users/a/AppData/Local/Devolutions/RemoteDesktopManagerG//"));
        assert!(rx(r"mRemoteNG-Installer-.*\.msi").is_match("mRemoteNG-Installer-1.2.msi"));
    }

    #[test]
    fn constructs() {
        assert!(rx(r"\Qa.b\E").is_match("a.b"));
        assert!(!rx(r"\Qa.b\E").is_match("axb"));
        assert!(rx(r"\0101").is_match("A"));
        assert!(rx(r"A\d").is_match("A1"));
        assert!(rx(r"[]a]").is_match("]"));
        assert!(rx(r"(?<n>x)").is_match("x"));
        // Unsupported in Rust: existing back-references, possessive
        // quantifiers, look-ahead, class intersections.
        for bad in [r"(a)\1", r"a*+", r"(?=a)", r"[a-z&&[^b]]", r"\k<n>"] {
            assert!(translate(bad).is_none(), "{bad}");
        }
    }
}

//! Autopsy's `EnglishOnlyStream`: printable-ASCII runs.

use pm_core::ReadAt;

use super::MIN_CHARS;
use crate::{TextError, TextSink};

const READ_BUF_SIZE: usize = 65_536;
/// Output is pushed in pieces of about this size.
const FLUSH: usize = 64 * 1024;

/// Autopsy `StringExtract.isPrintableAscii`.
fn printable(b: u8) -> bool {
    (32..=126).contains(&b) || b == 9
}

pub(super) fn extract(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<(), TextError> {
    let size = src.size();
    let mut buf = vec![0u8; READ_BUF_SIZE];
    let mut out = String::with_capacity(FLUSH + 1024);
    let mut run = String::new();
    let mut single_zero = false;
    let mut off = 0u64;
    while off < size {
        let want = usize::try_from((size - off).min(READ_BUF_SIZE as u64)).unwrap_or(0);
        let n = src.read_at(off, buf.get_mut(..want).unwrap_or_default())?;
        if n == 0 {
            break;
        }
        off += n as u64;
        for &b in buf.get(..n).unwrap_or_default() {
            // One NUL inside a run is tolerated (UTF-16LE ASCII); a second
            // consecutive one ends the run.
            single_zero = b == 0 && !single_zero;
            if printable(b) {
                run.push(char::from(b));
            } else if !single_zero {
                if run.len() >= MIN_CHARS {
                    out.push_str(&run);
                    out.push('\n');
                }
                run.clear();
            }
        }
        if out.len() >= FLUSH {
            sink.push(&out)?;
            out.clear();
        }
    }
    if run.len() >= MIN_CHARS {
        out.push_str(&run);
    }
    if !out.is_empty() {
        sink.push(&out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::strings::{StringsConfig, extract_bytes};

    fn run(data: &[u8]) -> String {
        extract_bytes(data, &StringsConfig::default())
    }

    #[test]
    fn printable_runs() {
        assert_eq!(
            run(b"\x01\x02hello\x03ab\x04world wide\x05"),
            "hello\nworld wide\n"
        );
        assert_eq!(run(b"abc"), "");
        assert_eq!(run(b"\xFFtail"), "tail");
        assert_eq!(run(b"tab\there\x00\x00"), "tab\there\n");
    }

    #[test]
    fn single_nul_is_tolerated() {
        assert_eq!(run(b"a\0b\0c\0d\0\0\0"), "abcd\n");
        assert_eq!(run(b"ab\0\0cd\x01"), "");
    }

    #[test]
    fn runs_cross_read_blocks() {
        let mut data = vec![0u8; super::READ_BUF_SIZE - 2];
        data.extend(b"split");
        data.push(1);
        assert_eq!(run(&data), "split\n");
    }
}

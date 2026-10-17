//! Autopsy's `StringExtract` (international strings).

use pm_core::ReadAt;

use super::{COMMON, MIN_CHARS, NONE, StringsConfig, script_of};
use crate::{TextError, TextSink};

const FILE_BUF_SIZE: usize = 1024 * 1024;

/// One decoded run starting at a given offset.
#[derive(Debug, Default)]
struct Run {
    text: String,
    chars: usize,
    bytes: usize,
    /// The decode stopped because the data ended, not at a bad character
    /// (more data might extend it).
    hit_end: bool,
}

/// Script check shared by the decoders; returns false to stop the run.
fn accept(cfg: &StringsConfig, locked: &mut u8, cp: u32) -> bool {
    let script = script_of(cp);
    if script == NONE {
        return false;
    }
    let generic = script == COMMON;
    if !generic && !cfg.enabled(script) {
        return false;
    }
    if *locked == NONE && !generic {
        *locked = script;
    }
    *locked == script || generic
}

fn decode_utf16(buf: &[u8], start: usize, big_endian: bool, cfg: &StringsConfig) -> Run {
    let mut r = Run::default();
    let mut locked = NONE;
    let mut off = start;
    loop {
        let (Some(&a), Some(&b)) = (buf.get(off), buf.get(off + 1)) else {
            r.hit_end = true;
            break;
        };
        off += 2;
        let unit = if big_endian {
            u16::from_be_bytes([a, b])
        } else {
            u16::from_le_bytes([a, b])
        };
        let cp = u32::from(unit);
        if !accept(cfg, &mut locked, cp) {
            break;
        }
        r.bytes += 2;
        r.chars += 1;
        // Surrogate units are never accepted (the table maps them to NONE).
        r.text
            .push(char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    r
}

/// Autopsy's hand-written UTF-8 decoder (BMP only: four-byte sequences
/// decode but end the run, as the script table has no entry for them).
fn decode_utf8(buf: &[u8], start: usize, cfg: &StringsConfig) -> Run {
    let mut r = Run::default();
    let mut locked = NONE;
    let mut off = start;
    let cont = |i: usize, lo: u8, hi: u8| buf.get(i).is_some_and(|&b| (lo..=hi).contains(&b));
    let at = |i: usize| buf.get(i).copied().unwrap_or(0);
    loop {
        let Some(&b0) = buf.get(off) else {
            r.hit_end = true;
            break;
        };
        let rest = buf.len() - off;
        let (len, cp) = match b0 {
            0x00..=0x7F => (1, u32::from(b0)),
            0xC2..=0xDF => {
                if rest < 2 {
                    r.hit_end = true;
                    break;
                }
                if !cont(off + 1, 0x80, 0xBF) {
                    break;
                }
                (
                    2,
                    (u32::from(b0 & 0x1F) << 6) | u32::from(at(off + 1) & 0x3F),
                )
            }
            0xE0..=0xEF => {
                if rest < 3 {
                    r.hit_end = true;
                    break;
                }
                let (lo, hi) = match b0 {
                    0xE0 => (0xA0, 0xBF),
                    0xED => (0x80, 0x9F),
                    _ => (0x80, 0xBF),
                };
                if !(cont(off + 1, lo, hi) && cont(off + 2, 0x80, 0xBF)) {
                    break;
                }
                (
                    3,
                    (u32::from(b0 & 0x0F) << 12)
                        | (u32::from(at(off + 1) & 0x3F) << 6)
                        | u32::from(at(off + 2) & 0x3F),
                )
            }
            0xF0..=0xF3 => {
                if rest < 4 {
                    r.hit_end = true;
                    break;
                }
                let lo = if b0 == 0xF0 { 0x90 } else { 0x80 };
                if !(cont(off + 1, lo, 0xBF)
                    && cont(off + 2, 0x80, 0xBF)
                    && cont(off + 3, 0x80, 0xBF))
                {
                    break;
                }
                // Beyond the BMP: never in the table.
                break;
            }
            _ => break,
        };
        off += len;
        if !accept(cfg, &mut locked, cp) {
            break;
        }
        r.bytes += len;
        r.chars += 1;
        r.text
            .push(char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    r
}

/// Scans `buf` (which starts at absolute offset `abs`); returns the text
/// found and how many bytes were consumed. Unless `last`, scanning stops
/// at a run that may continue in the next block, which is re-read then.
fn scan(buf: &[u8], abs: u64, cfg: &StringsConfig, last: bool, out: &mut String) -> usize {
    let len = buf.len();
    let mut cur = 0usize;
    while cur < len {
        if buf.get(cur) == Some(&0) && buf.get(cur + 1) == Some(&0) {
            cur += 2;
            continue;
        }
        let even = (abs + cur as u64).is_multiple_of(2);
        let r16 = (cfg.utf16 && even).then(|| {
            let be = decode_utf16(buf, cur, true, cfg);
            let le = decode_utf16(buf, cur, false, cfg);
            if be.chars > le.chars { be } else { le }
        });
        let r8 = cfg.utf8.then(|| decode_utf8(buf, cur, cfg));
        let win = match (r16, r8) {
            (Some(a), Some(b)) => {
                if a.chars > b.chars {
                    a
                } else {
                    b
                }
            }
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return len,
        };
        if win.hit_end && !last && cur > 0 {
            // May grow with the next block: re-scan from here.
            return cur;
        }
        if win.chars >= MIN_CHARS {
            out.push_str(&win.text);
            out.push('\n');
            cur += win.bytes;
        } else if cfg.utf8 {
            cur += 1;
        } else {
            cur += 2;
        }
    }
    len
}

pub(super) fn extract(
    src: &dyn ReadAt,
    cfg: &StringsConfig,
    sink: &mut dyn TextSink,
) -> Result<(), TextError> {
    let size = src.size();
    let mut buf: Vec<u8> = Vec::with_capacity(FILE_BUF_SIZE * 2);
    let mut buf_abs = 0u64; // absolute offset of buf[0]
    let mut off = 0u64; // next byte to read
    let mut out = String::new();
    loop {
        let want = usize::try_from((size - off.min(size)).min(FILE_BUF_SIZE as u64)).unwrap_or(0);
        let old = buf.len();
        buf.resize(old + want, 0);
        let n = if want == 0 {
            0
        } else {
            src.read_at(off, buf.get_mut(old..).unwrap_or_default())?
        };
        buf.truncate(old + n);
        off += n as u64;
        let last = n == 0 || off >= size;
        let mut used = scan(&buf, buf_abs, cfg, last, &mut out);
        if !cfg.utf8 && used % 2 == 1 {
            // Keep UTF-16-only scanning on even offsets.
            used -= 1;
        }
        if !out.is_empty() {
            sink.push(&out)?;
            out.clear();
        }
        if last {
            return Ok(());
        }
        buf.drain(..used);
        buf_abs += used as u64;
    }
}

#[cfg(test)]
mod tests {
    use crate::strings::{Script, StringsConfig, extract_bytes};

    fn cfg(scripts: &[Script]) -> StringsConfig {
        StringsConfig {
            utf8: true,
            utf16: true,
            scripts: scripts.to_vec(),
        }
    }

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn utf8_and_utf16_runs() {
        let c = cfg(&[Script::Latin2]);
        let mut data = b"\x01\x02hello world\x03".to_vec();
        data.extend([0x00, 0x00]);
        data.extend(utf16le("wide text"));
        data.extend([0x00, 0x00, 0x05]);
        assert_eq!(extract_bytes(&data, &c), "hello world\nwide text\n");
    }

    #[test]
    fn script_locking() {
        // Cyrillic run then Latin run: two strings.
        let c = cfg(&[Script::Latin1, Script::Cyrillic]);
        assert_eq!(
            extract_bytes("привет мир hello".as_bytes(), &c),
            "привет мир \nhello\n"
        );
        // Cyrillic not enabled: only the Latin part.
        let c = cfg(&[Script::Latin2]);
        assert_eq!(extract_bytes("привет hello".as_bytes(), &c), " hello\n");
        // Latin-Basic letters lock the run; a Latin-Extended letter ends it.
        assert_eq!(extract_bytes("kayıt defteri".as_bytes(), &c), "t defteri\n");
    }

    #[test]
    fn utf16_big_endian_and_han() {
        let c = cfg(&[Script::Han]);
        let data: Vec<u8> = "中文字符串"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        assert_eq!(extract_bytes(&data, &c), "中文字符串\n");
    }

    #[test]
    fn strings_across_blocks_are_whole() {
        let c = cfg(&[Script::Latin2]);
        let mut data = vec![0xFFu8; super::FILE_BUF_SIZE - 3];
        data.extend(b"boundary");
        data.push(0xFF);
        assert_eq!(extract_bytes(&data, &c), "boundary\n");
    }

    #[test]
    fn utf16_only() {
        let c = StringsConfig {
            utf8: false,
            utf16: true,
            scripts: vec![Script::Latin2],
        };
        let mut data = b"ascii text\x01\x01".to_vec();
        data.extend(utf16le("only wide"));
        assert_eq!(extract_bytes(&data, &c), "only wide\n");
    }
}

//! Tar reader: v7, ustar, GNU (long names `L`/`K`, base-256 sizes, old
//! GNU times) and POSIX pax (`x`/`g` records: path, size, times).
//!
//! Headers are verified by checksum. After a damaged header the walk
//! resynchronises on the next block that holds a valid header, so the rest
//! of a corrupted or carved archive is still recovered.

use std::io::Read;

use pm_core::ReadAt;

use crate::driver::Driver;
use crate::io::{RangeReader, read_upto};
use crate::names::decode_default;
use crate::{ArchiveError, Entry, MemberEnd};

const BLOCK: u64 = 512;
/// Largest long-name or pax record block read into memory.
const MAX_META: u64 = 1 << 20;
/// How far ahead a damaged archive is searched for the next header.
const RESYNC_LIMIT: u64 = 64 << 20;

/// Parses an octal or base-256 numeric field.
fn number(field: &[u8]) -> Option<u64> {
    let first = *field.first()?;
    if first & 0x80 != 0 {
        // GNU base-256: big-endian, first byte's high bit is the marker.
        if first & 0x40 != 0 {
            return None; // negative
        }
        let mut v: u64 = u64::from(first & 0x3F);
        for &b in field.get(1..)? {
            v = v.checked_mul(256)?.checked_add(u64::from(b))?;
        }
        return Some(v);
    }
    field
        .iter()
        .skip_while(|&&b| b == b' ' || b == 0)
        .take_while(|&&b| (b'0'..=b'7').contains(&b))
        .try_fold(0u64, |acc, &b| {
            acc.checked_mul(8)?.checked_add(u64::from(b - b'0'))
        })
}

/// Whether a 512-byte block is a header with a valid checksum.
fn checksum_ok(h: &[u8]) -> bool {
    let Some(stored) = h.get(148..156).and_then(number) else {
        return false;
    };
    let mut unsigned: u64 = 0;
    let mut signed: i64 = 0;
    for (i, &b) in h.iter().enumerate().take(512) {
        let b = if (148..156).contains(&i) { b' ' } else { b };
        unsigned += u64::from(b);
        signed += i64::from(b as i8);
    }
    stored == unsigned || i64::try_from(stored).is_ok_and(|s| s == signed)
}

/// Signature check used by format detection.
pub(crate) fn looks_like_tar(head: &[u8]) -> bool {
    let Some(h) = head.get(..512) else {
        return false;
    };
    if h.iter().all(|&b| b == 0) {
        return false;
    }
    checksum_ok(h)
}

fn field_str(b: &[u8]) -> &[u8] {
    let end = memchr::memchr(0, b).unwrap_or(b.len());
    b.get(..end).unwrap_or_default()
}

/// Pending overrides from GNU long-name and pax records.
#[derive(Debug, Default)]
struct Pending {
    path: Option<Vec<u8>>,
    size: Option<u64>,
    mtime: Option<i64>,
    atime: Option<i64>,
    ctime: Option<i64>,
}

/// Applies pax `len key=value\n` records.
fn apply_pax(data: &[u8], p: &mut Pending) {
    let mut rest = data;
    while !rest.is_empty() {
        let Some(sp) = memchr::memchr(b' ', rest) else {
            break;
        };
        let Some(len) = std::str::from_utf8(rest.get(..sp).unwrap_or_default())
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            break;
        };
        let Some(rec) = rest.get(sp + 1..len) else {
            break;
        };
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(eq) = memchr::memchr(b'=', rec) {
            let key = rec.get(..eq).unwrap_or_default();
            let val = rec.get(eq + 1..).unwrap_or_default();
            let secs = || {
                std::str::from_utf8(val)
                    .ok()
                    .and_then(|v| v.split('.').next()?.parse::<i64>().ok())
            };
            match key {
                b"path" => p.path = Some(val.to_vec()),
                b"size" => {
                    p.size = std::str::from_utf8(val).ok().and_then(|v| v.parse().ok());
                }
                b"mtime" => p.mtime = secs(),
                b"atime" => p.atime = secs(),
                b"ctime" => p.ctime = secs(),
                _ => {}
            }
        }
        rest = rest.get(len..).unwrap_or_default();
    }
}

pub(crate) fn extract(src: &dyn ReadAt, d: &mut Driver<'_>) -> Result<(), ArchiveError> {
    let size = src.size();
    let mut pos = 0u64;
    let mut index = 0u64;
    let mut pending = Pending::default();
    let mut global = Pending::default();
    let mut zero_blocks = 0;
    while pos + BLOCK <= size {
        d.check_cancel()?;
        let h = read_upto(src, pos, 512)?;
        if h.iter().all(|&b| b == 0) {
            zero_blocks += 1;
            pos += BLOCK;
            if zero_blocks >= 2 {
                break;
            }
            continue;
        }
        zero_blocks = 0;
        if !checksum_ok(&h) {
            if index == 0 {
                return Err(ArchiveError::malformed("tar", "bad header checksum"));
            }
            d.warn(format!(
                "tar: damaged header at offset {pos}; resynchronising"
            ));
            match resync(src, pos + BLOCK)? {
                Some(p) => {
                    pos = p;
                    continue;
                }
                None => break,
            }
        }
        let typ = h.get(156).copied().unwrap_or(0);
        let hdr_size = h.get(124..136).and_then(number).unwrap_or(0);
        let data_start = pos + BLOCK;
        let mut data_len = hdr_size;
        match typ {
            b'L' | b'K' | b'x' | b'g' => {
                if hdr_size > MAX_META {
                    d.warn(format!("tar: oversized metadata record at {pos}"));
                } else {
                    let meta = read_upto(src, data_start, hdr_size as usize)?;
                    match typ {
                        b'L' => pending.path = Some(field_str(&meta).to_vec()),
                        b'x' => apply_pax(&meta, &mut pending),
                        b'g' => apply_pax(&meta, &mut global),
                        _ => {} // long link names are not needed
                    }
                }
                pos = data_start + hdr_size.div_ceil(BLOCK) * BLOCK;
                continue;
            }
            _ => {}
        }
        if let Some(s) = pending.size {
            data_len = s;
        }
        // Hard and symbolic links, devices and FIFOs have no data.
        let is_dir = typ == b'5' || typ == b'D';
        let has_data = matches!(typ, b'0' | 0 | b'7' | b'S');
        if !has_data {
            data_len = if is_dir { 0 } else { data_len.min(hdr_size) };
        }
        let ustar = h.get(257..262) == Some(&b"ustar"[..]);
        let gnu = h.get(257..265) == Some(&b"ustar  \0"[..]);
        let mut name = field_str(h.get(0..100).unwrap_or_default()).to_vec();
        if ustar && !gnu {
            let prefix = field_str(h.get(345..500).unwrap_or_default());
            if !prefix.is_empty() {
                let mut full = prefix.to_vec();
                full.push(b'/');
                full.extend_from_slice(&name);
                name = full;
            }
        }
        let path = pending
            .path
            .take()
            .or_else(|| global.path.clone())
            .unwrap_or(name);
        let mtime = pending.mtime.or(global.mtime).or_else(|| {
            h.get(136..148)
                .and_then(number)
                .and_then(|v| i64::try_from(v).ok())
        });
        let (gnu_atime, gnu_ctime) = if gnu {
            (
                h.get(345..357)
                    .and_then(number)
                    .and_then(|v| i64::try_from(v).ok()),
                h.get(357..369)
                    .and_then(number)
                    .and_then(|v| i64::try_from(v).ok()),
            )
        } else {
            (None, None)
        };
        let atime = pending
            .atime
            .or(global.atime)
            .or(gnu_atime)
            .filter(|&t| t != 0);
        // pax/GNU ctime is the inode change time, not creation; tar has no
        // creation time.
        let _ = pending.ctime.or(global.ctime).or(gnu_ctime);
        pending = Pending::default();
        let entry = Entry {
            index,
            path: decode_default(&path),
            raw_path: (std::str::from_utf8(&path).is_err()).then(|| path.clone()),
            is_dir: is_dir || path.ends_with(b"/"),
            size: Some(data_len),
            packed_size: Some(data_len),
            encrypted: false,
            modified: mtime.filter(|&t| t != 0),
            accessed: atime,
            created: None,
        };
        index += 1;
        if has_data && data_start.saturating_add(data_len) > size {
            d.warn(format!("tar: {} is truncated", entry.path));
        }
        let mut r: Box<dyn Read> = Box::new(RangeReader::new(src, data_start, data_len));
        let reader: Option<&mut dyn Read> = if has_data { Some(&mut r) } else { None };
        let end = d.member(&entry, reader, None)?;
        if let MemberEnd::Failed(_) = end {
            break;
        }
        pos = data_start.saturating_add(data_len.div_ceil(BLOCK).saturating_mul(BLOCK));
    }
    Ok(())
}

/// Finds the next block with a valid header.
fn resync(src: &dyn ReadAt, from: u64) -> Result<Option<u64>, ArchiveError> {
    let end = src.size().min(from.saturating_add(RESYNC_LIMIT));
    let mut pos = from;
    while pos + BLOCK <= end {
        let chunk = read_upto(src, pos, 1 << 20)?;
        let mut off = 0usize;
        while let Some(h) = chunk.get(off..off + 512) {
            if !h.iter().all(|&b| b == 0) && checksum_ok(h) {
                return Ok(Some(pos + off as u64));
            }
            off += 512;
        }
        if chunk.len() < 512 {
            break;
        }
        pos += (chunk.len() / 512 * 512) as u64;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(number(b"0000644\0"), Some(0o644));
        assert_eq!(number(b"  777 \0"), Some(0o777));
        assert_eq!(number(b"\0\0\0"), Some(0));
        assert_eq!(
            number(&[0x80, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]),
            Some(1 << 32)
        );
        assert_eq!(number(&[0xFF; 12]), None);
        assert_eq!(
            number(&[0x80, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
            None
        );
        assert_eq!(number(b""), None);
    }

    #[test]
    fn pax_records() {
        let mut p = Pending::default();
        apply_pax(
            b"29 path=a/very/long/name.txt\n22 mtime=1600000000.5\n13 size=1234\n",
            &mut p,
        );
        assert_eq!(p.path.as_deref(), Some(&b"a/very/long/name.txt"[..]));
        assert_eq!(p.mtime, Some(1_600_000_000));
        assert_eq!(p.size, Some(1234));
        // Malformed lengths stop parsing without panicking.
        apply_pax(b"999 path=x\n", &mut p);
        apply_pax(b"x", &mut p);
        apply_pax(b"0 a=b\n", &mut p);
    }
}

//! ISO 9660 reader with Joliet (preferred, as 7-Zip does) and Rock Ridge
//! (`NM`) names, multi-extent files and recording dates.

use std::collections::HashSet;
use std::io::Read;

use pm_core::{Bytes, ReadAt};

use crate::driver::Driver;
use crate::io::{RangeReader, read_upto};
use crate::{ArchiveError, Entry};

const SECTOR: u64 = 2048;
const MAX_DEPTH: usize = 64;
/// Largest directory read into memory.
const MAX_DIR_BYTES: u64 = 64 << 20;

/// `CD001` at the first volume descriptor.
pub(crate) fn is_iso(src: &dyn ReadAt) -> bool {
    read_upto(src, 16 * SECTOR + 1, 5).is_ok_and(|b| b == b"CD001")
}

/// A volume's root directory and name style.
#[derive(Debug, Clone, Copy)]
struct Volume {
    root_lba: u64,
    root_len: u64,
    block: u64,
    joliet: bool,
}

/// One directory record.
#[derive(Debug, Clone)]
struct Record {
    lba: u64,
    len: u64,
    flags: u8,
    name: String,
    time: Option<i64>,
}

fn volumes(src: &dyn ReadAt) -> (Option<Volume>, Option<Volume>) {
    let mut pvd = None;
    let mut joliet = None;
    for i in 16..16 + 64u64 {
        let Ok(vd) = read_upto(src, i * SECTOR, 2048) else {
            break;
        };
        if vd.len() < 190 || vd.get(1..6) != Some(&b"CD001"[..]) {
            break;
        }
        let typ = vd.first().copied().unwrap_or(0);
        if typ == 255 {
            break;
        }
        let block = u64::from(vd.u16_le(128).unwrap_or(2048)).clamp(512, 2048);
        let root = vd.get(156..190).unwrap_or_default();
        let v = Volume {
            root_lba: u64::from(root.u32_le(2).unwrap_or(0)),
            root_len: u64::from(root.u32_le(10).unwrap_or(0)),
            block,
            joliet: false,
        };
        match typ {
            1 if pvd.is_none() => pvd = Some(v),
            2 => {
                let esc = vd.get(88..120).unwrap_or_default();
                let is_joliet = [&b"%/@"[..], b"%/C", b"%/E"]
                    .iter()
                    .any(|s| memchr::memmem::find(esc, s).is_some());
                if is_joliet && joliet.is_none() {
                    joliet = Some(Volume { joliet: true, ..v });
                }
            }
            _ => {}
        }
    }
    (pvd, joliet)
}

/// ISO 9660 7-byte recording date as Unix seconds.
fn record_time(b: &[u8]) -> Option<i64> {
    let year = 1900 + i16::from(*b.first()?);
    let (mo, d, h, mi, s) = (
        i8::try_from(*b.get(1)?).ok()?,
        i8::try_from(*b.get(2)?).ok()?,
        i8::try_from(*b.get(3)?).ok()?,
        i8::try_from(*b.get(4)?).ok()?,
        i8::try_from(*b.get(5)?).ok()?,
    );
    let offset_q = i64::from(*b.get(6)? as i8);
    if mo == 0 || d == 0 {
        return None;
    }
    let dt = jiff::civil::DateTime::new(year, mo, d, h, mi, s, 0).ok()?;
    let utc = dt
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?
        .timestamp()
        .as_second();
    Some(utc - offset_q * 15 * 60)
}

/// The Rock Ridge alternate name in a system-use area, if any.
fn rock_ridge_name(su: &[u8]) -> Option<String> {
    let mut p = 0usize;
    let mut name = Vec::new();
    let mut found = false;
    while p + 4 <= su.len() {
        let sig = su.get(p..p + 2)?;
        let len = usize::from(*su.get(p + 2)?);
        if len < 4 {
            break;
        }
        let body = su.get(p + 4..p + len)?;
        if sig == b"NM" {
            let flags = *body.first()?;
            if flags & 0x06 == 0 {
                name.extend_from_slice(body.get(1..)?);
                found = true;
            }
        }
        p += len;
    }
    found.then(|| String::from_utf8_lossy(&name).into_owned())
}

fn clean_name(raw: &str) -> String {
    let base = raw.split(';').next().unwrap_or(raw);
    base.strip_suffix('.').unwrap_or(base).to_owned()
}

/// Reads the records of a directory.
fn read_dir(src: &dyn ReadAt, v: Volume, lba: u64, len: u64) -> Vec<Record> {
    let len = len.min(MAX_DIR_BYTES);
    let Ok(data) = read_upto(src, lba.saturating_mul(v.block), len as usize) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < data.len() {
        let rlen = usize::from(data.get(p).copied().unwrap_or(0));
        if rlen == 0 {
            // Records do not cross sector boundaries: skip the padding.
            let blk = usize::try_from(v.block).unwrap_or(2048);
            let next = (p / blk + 1) * blk;
            if next <= p {
                break;
            }
            p = next;
            continue;
        }
        let Some(r) = data.get(p..p + rlen) else {
            break;
        };
        p += rlen;
        if r.len() < 33 {
            continue;
        }
        let name_len = usize::from(r.get(32).copied().unwrap_or(0));
        let Some(raw) = r.get(33..33 + name_len) else {
            continue;
        };
        if raw == [0] || raw == [1] {
            continue;
        }
        let name = if v.joliet {
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| c.u16_be(0).unwrap_or(0))
                .collect();
            clean_name(&String::from_utf16_lossy(&units))
        } else {
            let pad = usize::from(name_len % 2 == 0);
            let su = r.get(33 + name_len + pad..).unwrap_or_default();
            rock_ridge_name(su).unwrap_or_else(|| clean_name(&String::from_utf8_lossy(raw)))
        };
        out.push(Record {
            lba: u64::from(r.u32_le(2).unwrap_or(0)),
            len: u64::from(r.u32_le(10).unwrap_or(0)),
            flags: r.get(25).copied().unwrap_or(0),
            name,
            time: r.get(18..25).and_then(record_time),
        });
    }
    out
}

/// A reader over consecutive extents.
struct Extents<'a> {
    src: &'a dyn ReadAt,
    parts: std::vec::IntoIter<(u64, u64)>,
    cur: Option<RangeReader<'a>>,
}

impl Read for Extents<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(r) = self.cur.as_mut() {
                let n = r.read(buf)?;
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
            }
            match self.parts.next() {
                Some((off, len)) => self.cur = Some(RangeReader::new(self.src, off, len)),
                None => return Ok(0),
            }
        }
    }
}

pub(crate) fn extract(src: &dyn ReadAt, d: &mut Driver<'_>) -> Result<(), ArchiveError> {
    let (pvd, joliet) = volumes(src);
    let Some(v) = joliet.or(pvd) else {
        return Err(ArchiveError::malformed("ISO 9660", "no volume descriptor"));
    };
    let mut visited = HashSet::new();
    let mut stack = vec![(v.root_lba, v.root_len, String::new(), 0usize)];
    let mut index = 0u64;
    while let Some((lba, len, prefix, depth)) = stack.pop() {
        d.check_cancel()?;
        if !visited.insert(lba) {
            d.warn(format!("ISO 9660: directory loop at block {lba}"));
            continue;
        }
        let records = read_dir(src, v, lba, len);
        let mut pending: Vec<(u64, u64)> = Vec::new();
        let mut subdirs = Vec::new();
        for r in records {
            let path = format!("{prefix}{}", r.name);
            let offset = r.lba.saturating_mul(v.block);
            if r.flags & 0x02 != 0 {
                let e = Entry {
                    index,
                    path: format!("{path}/"),
                    is_dir: true,
                    modified: r.time,
                    ..Entry::default()
                };
                index += 1;
                d.member(&e, None, None)?;
                if depth + 1 < MAX_DEPTH {
                    subdirs.push((r.lba, r.len, format!("{path}/"), depth + 1));
                } else {
                    d.warn(format!("ISO 9660: {path} is nested too deeply"));
                }
                continue;
            }
            pending.push((offset, r.len));
            if r.flags & 0x80 != 0 {
                // Multi-extent: more records of this file follow.
                continue;
            }
            let parts = std::mem::take(&mut pending);
            let size: u64 = parts.iter().map(|p| p.1).sum();
            let e = Entry {
                index,
                path,
                is_dir: false,
                size: Some(size),
                packed_size: Some(size),
                modified: r.time,
                ..Entry::default()
            };
            index += 1;
            let mut rd = Extents {
                src,
                parts: parts.into_iter(),
                cur: None,
            };
            d.member(&e, Some(&mut rd), None)?;
        }
        // Depth-first in name order.
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_times() {
        assert_eq!(clean_name("README.TXT;1"), "README.TXT");
        assert_eq!(clean_name("NOEXT.;1"), "NOEXT");
        assert_eq!(record_time(&[120, 1, 2, 3, 4, 5, 0]), Some(1_577_934_245));
        assert_eq!(
            record_time(&[120, 1, 2, 3, 4, 5, 4]),
            Some(1_577_934_245 - 3600)
        );
        assert_eq!(record_time(&[0, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(record_time(&[1, 2]), None);
        let mut su = b"NM\x09\x01\x00abcd".to_vec();
        su.extend(b"PX\x04\x01");
        assert_eq!(rock_ridge_name(&su).as_deref(), Some("abcd"));
        assert_eq!(rock_ridge_name(b"NM\x02"), None);
    }
}

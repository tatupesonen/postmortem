//! `AppCompatCache` ("`ShimCache`") from the SYSTEM hive: executables the
//! application-compatibility engine looked at, with their last-modified
//! time and (Vista–8.1) an execution flag. Formats per
//! `AppCompatCacheParser` (`AppCompatCache/*.cs`, MIT): XP (`0xDEADBEEF`),
//! 2003/Vista/2008 (`0xBADC0FFE`), 7/2008 R2 (`0xBADC0FEE`), 8.0 (`00ts`),
//! 8.1 (`10ts` at 128) and 10/11 (`10ts` after a 0x30/0x34 header).

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;
use pm_registry::Hive;

use super::sub;
use super::system::{current_control_set, is_32bit};

const MAX_ENTRIES: usize = 100_000;

/// One cache entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimCacheEntry {
    /// Position in the cache (0 = most recent).
    pub position: usize,
    pub path: String,
    /// File last-modified time (Unix seconds).
    pub last_modified: Option<i64>,
    /// Execution flag where the format has one.
    pub executed: Option<bool>,
}

/// The cache of the current control set.
pub fn shimcache(h: &Hive) -> Vec<ShimCacheEntry> {
    let Some(cs) = current_control_set(h) else {
        return Vec::new();
    };
    let raw = sub(&cs, "Control\\Session Manager\\AppCompatCache")
        .and_then(|k| k.binary_value("AppCompatCache"))
        .or_else(|| {
            sub(&cs, "Control\\Session Manager\\AppCompatibility")
                .and_then(|k| k.binary_value("AppCompatCache"))
        });
    raw.map(|r| parse(&r, is_32bit(h))).unwrap_or_default()
}

/// Parses a raw `AppCompatCache` value.
pub fn parse(b: &[u8], is_32bit: bool) -> Vec<ShimCacheEntry> {
    let sig = b.u32_le(0).unwrap_or(0);
    let at128 = b.get(128..132);
    let mut out = Vec::new();
    match sig {
        0xDEAD_BEEF => xp(b, &mut out),
        0xBADC_0FFE => vista(b, is_32bit, &mut out),
        0xBADC_0FEE => win7(b, is_32bit, &mut out),
        _ if at128 == Some(b"00ts") || at128 == Some(b"10ts") => {
            let sig: [u8; 4] = at128.and_then(|s| s.try_into().ok()).unwrap_or_default();
            win8(b, 128, sig, &mut out);
        }
        _ => {
            let start = sig as usize;
            if b.get(start..start + 4) == Some(b"10ts") {
                win10(b, start, &mut out);
            }
        }
    }
    out
}

fn clean(p: &str) -> String {
    p.replace("\\??\\", "")
}

fn ft(b: &[u8], at: usize) -> Option<i64> {
    b.u64_le(at).ok().and_then(crate::filetime_unix)
}

fn xp(b: &[u8], out: &mut Vec<ShimCacheEntry>) {
    let count = (b.u32_le(4).unwrap_or(0) as usize).min(MAX_ENTRIES);
    let mut at = 400;
    while out.len() < count {
        let Some(e) = b.get(at..at + 552) else { break };
        out.push(ShimCacheEntry {
            position: out.len(),
            path: clean(utf16le_lossy(e.get(..528).unwrap_or_default()).trim()),
            last_modified: ft(e, 528),
            executed: None,
        });
        at += 552;
    }
}

fn path_at(b: &[u8], off: usize, len: usize) -> String {
    clean(&b.get(off..off + len).map(utf16le_lossy).unwrap_or_default())
}

fn vista(b: &[u8], is_32bit: bool, out: &mut Vec<ShimCacheEntry>) {
    let count = (b.u32_le(4).unwrap_or(0) as usize).min(MAX_ENTRIES);
    let (size, step) = if is_32bit { (24, 24) } else { (32, 32) };
    let mut at = 8;
    while out.len() < count {
        let Some(e) = b.get(at..at + size) else { break };
        let len = usize::from(e.u16_le(0).unwrap_or(0));
        let (off, t_at, flags_at) = if is_32bit {
            (e.u32_le(4).unwrap_or(0) as usize, 8, 16)
        } else {
            (e.u64_le(8).unwrap_or(0) as usize, 16, 24)
        };
        let flags = e.u32_le(flags_at).unwrap_or(0);
        out.push(ShimCacheEntry {
            position: out.len(),
            path: path_at(b, off, len),
            last_modified: ft(e, t_at),
            executed: (!is_32bit).then_some(flags & 2 != 0),
        });
        at += step;
    }
}

fn win7(b: &[u8], is_32bit: bool, out: &mut Vec<ShimCacheEntry>) {
    let count = (b.u32_le(4).unwrap_or(0) as usize).min(MAX_ENTRIES);
    let step = if is_32bit { 32 } else { 48 };
    let mut at = 128;
    while out.len() < count {
        let Some(e) = b.get(at..at + step) else { break };
        let len = usize::from(e.u16_le(0).unwrap_or(0));
        let (off, t_at, flags_at) = if is_32bit {
            (e.u32_le(4).unwrap_or(0) as usize, 8, 16)
        } else {
            (e.u64_le(8).unwrap_or(0) as usize, 16, 24)
        };
        out.push(ShimCacheEntry {
            position: out.len(),
            path: path_at(b, off, len),
            last_modified: ft(e, t_at),
            executed: Some(e.u32_le(flags_at).unwrap_or(0) & 2 != 0),
        });
        at += step;
    }
}

fn win8(b: &[u8], mut at: usize, sig: [u8; 4], out: &mut Vec<ShimCacheEntry>) {
    while out.len() < MAX_ENTRIES && b.get(at..at + 4) == Some(&sig[..]) {
        let start = at;
        at += 12;
        let Ok(plen) = b.u16_le(at) else { break };
        let path = path_at(b, at + 2, usize::from(plen));
        at += 2 + usize::from(plen);
        let Ok(pkg) = b.u16_le(at) else { break };
        at += 2 + usize::from(pkg);
        let flags = b.u32_le(at).unwrap_or(0);
        at += 8;
        let t = ft(b, at);
        at += 8;
        let Ok(dlen) = b.u32_le(at) else { break };
        at += 4 + dlen as usize;
        out.push(ShimCacheEntry {
            position: out.len(),
            path,
            last_modified: t,
            executed: Some(flags & 2 != 0),
        });
        if at <= start {
            break;
        }
    }
}

fn win10(b: &[u8], mut at: usize, out: &mut Vec<ShimCacheEntry>) {
    while out.len() < MAX_ENTRIES && b.get(at..at + 4) == Some(b"10ts") {
        let start = at;
        at += 12;
        let Ok(plen) = b.u16_le(at) else { break };
        let path = path_at(b, at + 2, usize::from(plen));
        at += 2 + usize::from(plen);
        let t = ft(b, at);
        at += 8;
        let Ok(dlen) = b.u32_le(at) else { break };
        let data = b.get(at + 4..at + 4 + dlen as usize).unwrap_or_default();
        at += 4 + dlen as usize;
        let executed = data
            .len()
            .checked_sub(4)
            .and_then(|i| data.u32_le(i).ok())
            .map(|v| v == 1);
        out.push(ShimCacheEntry {
            position: out.len(),
            path,
            last_modified: t,
            executed,
        });
        if at <= start {
            break;
        }
    }
}

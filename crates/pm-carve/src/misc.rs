//! SQLite databases, Windows executables, Windows XML event logs.

use crate::Verdict;
use crate::window::Window;

/// SQLite: page size × the in-header page count (valid only when the
/// "version-valid-for" number matches the change counter, as SQLite
/// itself requires).
pub(crate) fn sqlite(w: &mut Window<'_>, start: u64) -> Verdict {
    let (Some(ps), Some(change), Some(pages), Some(valid_for)) = (
        w.u16be(start + 16),
        w.u32be(start + 24),
        w.u32be(start + 28),
        w.u32be(start + 92),
    ) else {
        return Verdict::No;
    };
    let page_size: u64 = if ps == 1 { 65_536 } else { u64::from(ps) };
    if !page_size.is_power_of_two() || !(512..=65_536).contains(&page_size) {
        return Verdict::No;
    }
    if pages == 0 || valid_for != change {
        return Verdict::No;
    }
    let len = page_size * u64::from(pages);
    if start + len > w.end() {
        return Verdict::Truncated {
            len: w.end() - start,
            ext: "sqlite",
        };
    }
    Verdict::Found { len, ext: "sqlite" }
}

/// Windows PE executables: the end of the furthest section or of the
/// signature (security directory), at least the headers.
pub(crate) fn exe(w: &mut Window<'_>, start: u64) -> Verdict {
    let Some(lfanew) = w.u32le(start + 0x3C) else {
        return Verdict::No;
    };
    let pe = u64::from(lfanew);
    if !(0x40..0x10000).contains(&pe) || w.get(start + pe, 4) != Some(b"PE\0\0") {
        return Verdict::No;
    }
    let coff = start + pe + 4;
    let (Some(n_sections), Some(opt_size), Some(chars), Some(magic)) = (
        w.u16le(coff + 2),
        w.u16le(coff + 16),
        w.u16le(coff + 18),
        w.u16le(coff + 20),
    ) else {
        return Verdict::No;
    };
    if n_sections == 0 || n_sections > 96 || !matches!(magic, 0x10B | 0x20B) {
        return Verdict::No;
    }
    let opt = coff + 20;
    let Some(headers) = w.u32le(opt + 60) else {
        return Verdict::No;
    };
    let mut end = u64::from(headers);
    let table = opt + u64::from(opt_size);
    for i in 0..u64::from(n_sections) {
        let s = table + 40 * i;
        let (Some(raw_size), Some(raw_ptr)) = (w.u32le(s + 16), w.u32le(s + 20)) else {
            return Verdict::No;
        };
        if raw_size > 0 {
            end = end.max(u64::from(raw_ptr) + u64::from(raw_size));
        }
    }
    // Security directory (index 4): a file offset, not an RVA.
    let dirs = opt + if magic == 0x20B { 112 } else { 96 };
    let n_dirs = w
        .u32le(opt + if magic == 0x20B { 108 } else { 92 })
        .unwrap_or(0);
    if n_dirs > 4
        && let (Some(off), Some(size)) = (w.u32le(dirs + 32), w.u32le(dirs + 36))
        && off > 0
        && size > 0
    {
        end = end.max(u64::from(off) + u64::from(size));
    }
    let ext = if chars & 0x2000 != 0 { "dll" } else { "exe" };
    if start + end > w.end() {
        return Verdict::Truncated {
            len: w.end() - start,
            ext,
        };
    }
    Verdict::Found { len: end, ext }
}

/// EVTX: the file header names the number of 64 KiB chunks; only chunks
/// that carry the chunk signature count.
pub(crate) fn evtx(w: &mut Window<'_>, start: u64) -> Verdict {
    let (Some(header_size), Some(block), Some(chunks)) = (
        w.u32le(start + 32),
        w.u16le(start + 40),
        w.u16le(start + 42),
    ) else {
        return Verdict::No;
    };
    if header_size != 128 || block != 4096 {
        return Verdict::No;
    }
    let mut n = 0u64;
    while n < u64::from(chunks) && w.get(start + 4096 + n * 65_536, 8) == Some(b"ElfChnk\0") {
        n += 1;
    }
    let len = 4096 + n * 65_536;
    if n < u64::from(chunks) {
        return Verdict::Truncated { len, ext: "evtx" };
    }
    Verdict::Found { len, ext: "evtx" }
}

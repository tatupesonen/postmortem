//! Transaction-log replay for dirty hives.
//!
//! * **Old format** (Windows XP – 8): the log holds a copy of the base
//!   block, a `DIRT` dirty vector (one bit per 512-byte sector of the hive
//!   bins) and the dirty sectors in bit order.
//! * **New format** (Windows 8.1+): after the base block come `HvLE` log
//!   entries, each with a sequence number, dirty-page references
//!   (offset, size) and the pages; entries are authenticated with two
//!   Marvin32 hashes. Entries are applied in sequence order starting at the
//!   hive's secondary sequence number, stopping at the first gap or bad
//!   hash.

use pm_core::{Bytes, SharedSource};

use crate::base_block::{BASE_BLOCK_SIZE, BaseBlock, HiveFileType, fix_checksum};
use crate::data::{HiveData, SECTOR};

/// Marvin32 seed used by the registry.
const MARVIN_SEED: u64 = 0x82EF_4D88_7A4E_55C5;
/// Largest hive-bins size accepted from a log header (2 GiB).
const MAX_HBINS: u32 = 0x8000_0000;
/// Largest log read into memory at once per entry.
const MAX_ENTRY: usize = 256 << 20;

/// What a replay changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayReport {
    /// 512-byte sectors applied from an old-format log.
    pub old_format_sectors: usize,
    /// `HvLE` entries applied from new-format logs.
    pub new_format_entries: usize,
    /// Pages (of any size) applied from new-format logs.
    pub new_format_pages: usize,
    /// Logs that were not usable (bad header, unknown format).
    pub ignored_logs: usize,
}

impl ReplayReport {
    /// Whether anything was applied.
    pub fn applied(&self) -> bool {
        self.old_format_sectors > 0 || self.new_format_entries > 0
    }
}

/// One decoded log.
enum Log {
    Old {
        header: BaseBlock,
        raw_base: Vec<u8>,
        src: SharedSource,
        dirt: u64,
    },
    New {
        header: BaseBlock,
        raw_base: Vec<u8>,
        entries: Vec<Entry>,
    },
}

/// A verified new-format log entry.
struct Entry {
    seq: u32,
    hbins_size: u32,
    pages: Vec<(u32, Vec<u8>)>,
}

/// Replays `logs` into `data` (the hive described by `hive`).
pub(crate) fn replay(data: &mut HiveData, hive: &BaseBlock, logs: &[SharedSource]) -> ReplayReport {
    let mut report = ReplayReport::default();
    let mut old = Vec::new();
    let mut new_entries: Vec<Entry> = Vec::new();
    let mut new_base: Option<(BaseBlock, Vec<u8>)> = None;
    for src in logs {
        match open_log(src) {
            Some(Log::Old {
                header,
                raw_base,
                src,
                dirt,
            }) => old.push((header, raw_base, src, dirt)),
            Some(Log::New {
                header,
                raw_base,
                entries,
            }) => {
                if new_base
                    .as_ref()
                    .is_none_or(|(b, _)| header.secondary_seq > b.secondary_seq)
                {
                    new_base = Some((header, raw_base));
                }
                new_entries.extend(entries);
            }
            None => report.ignored_logs += 1,
        }
    }
    if !new_entries.is_empty() {
        apply_new(data, hive, new_entries, new_base, &mut report);
    } else if let Some((header, raw_base, src, dirt)) =
        old.into_iter().max_by_key(|(h, ..)| h.secondary_seq)
    {
        apply_old(data, &header, raw_base, &src, dirt, &mut report);
    }
    if report.applied() {
        tracing::info!(?report, "replayed registry transaction logs");
    }
    report
}

fn open_log(src: &SharedSource) -> Option<Log> {
    let size = src.size();
    let head = src
        .read_vec_at(0, (BASE_BLOCK_SIZE as u64).min(size) as usize)
        .ok()?;
    let header = BaseBlock::parse(&head).ok()?;
    if !header.checksum_ok {
        tracing::debug!("transaction log base block checksum mismatch; ignored");
        return None;
    }
    let mut raw_base = head.get(..SECTOR).unwrap_or_default().to_vec();
    raw_base.resize(BASE_BLOCK_SIZE, 0);
    let sig_at = |off: u64| -> Option<[u8; 4]> {
        let v = src.read_vec_at(off, 4).ok()?;
        v.array::<4>(0).ok()
    };
    if header.file_type == HiveFileType::LogNew || sig_at(512) == Some(*b"HvLE") {
        let entries = read_entries(src, 512);
        return Some(Log::New {
            header,
            raw_base,
            entries,
        });
    }
    for dirt in [512u64, BASE_BLOCK_SIZE as u64] {
        if sig_at(dirt) == Some(*b"DIRT") {
            return Some(Log::Old {
                header,
                raw_base,
                src: src.clone(),
                dirt,
            });
        }
    }
    None
}

/// Reads consecutive valid `HvLE` entries starting at `offset`.
fn read_entries(src: &SharedSource, mut offset: u64) -> Vec<Entry> {
    let mut out = Vec::new();
    let size = src.size();
    while offset + 40 <= size {
        let Ok(head) = src.read_vec_at(offset, 40) else {
            break;
        };
        if head.get(..4) != Some(b"HvLE") {
            break;
        }
        let len = head.u32_le(4).unwrap_or(0) as usize;
        if len < 40 || !len.is_multiple_of(SECTOR) || len > MAX_ENTRY {
            break;
        }
        let Ok(buf) = src.read_vec_at(offset, len) else {
            break;
        };
        let Some(e) = parse_entry(&buf) else {
            tracing::debug!(offset, "invalid transaction log entry; stopping");
            break;
        };
        out.push(e);
        offset += len as u64;
    }
    out
}

fn parse_entry(buf: &[u8]) -> Option<Entry> {
    let hash1 = buf.u64_le(24).ok()?;
    let hash2 = buf.u64_le(32).ok()?;
    if marvin32(buf.get(..32)?, MARVIN_SEED) != hash2
        || marvin32(buf.get(40..)?, MARVIN_SEED) != hash1
    {
        return None;
    }
    let seq = buf.u32_le(12).ok()?;
    let hbins_size = buf.u32_le(16).ok()?;
    let count = buf.u32_le(20).ok()? as usize;
    let refs_end = 40usize.checked_add(count.checked_mul(8)?)?;
    let mut data_at = refs_end;
    let mut pages = Vec::with_capacity(count.min(4096));
    for i in 0..count {
        let r = 40 + i * 8;
        let off = buf.u32_le(r).ok()?;
        let len = buf.u32_le(r + 4).ok()? as usize;
        let page = buf.get(data_at..data_at.checked_add(len)?)?;
        pages.push((off, page.to_vec()));
        data_at += len;
    }
    Some(Entry {
        seq,
        hbins_size,
        pages,
    })
}

fn apply_new(
    data: &mut HiveData,
    hive: &BaseBlock,
    mut entries: Vec<Entry>,
    log_base: Option<(BaseBlock, Vec<u8>)>,
    report: &mut ReplayReport,
) {
    entries.sort_by_key(|e| e.seq);
    entries.dedup_by_key(|e| e.seq);
    let mut expected = if hive.checksum_ok {
        Some(hive.secondary_seq)
    } else {
        log_base.as_ref().map(|(b, _)| b.secondary_seq)
    };
    let mut last: Option<(u32, u32)> = None;
    for e in entries {
        match expected {
            Some(x) if e.seq < x => continue,
            Some(x) if e.seq != x => break,
            _ => {}
        }
        if e.hbins_size > MAX_HBINS {
            break;
        }
        for (off, page) in &e.pages {
            data.patch(BASE_BLOCK_SIZE as u64 + u64::from(*off), page);
            report.new_format_pages += 1;
        }
        data.grow_to(BASE_BLOCK_SIZE as u64 + u64::from(e.hbins_size));
        report.new_format_entries += 1;
        expected = Some(e.seq.wrapping_add(1));
        last = Some((e.seq, e.hbins_size));
    }
    if let Some((seq, hbins)) = last {
        let mut base = if hive.checksum_ok {
            data.base().to_vec()
        } else {
            log_base.map_or_else(|| data.base().to_vec(), |(_, raw)| raw)
        };
        finish_base(&mut base, seq.wrapping_add(1), hbins);
        data.set_base(base);
    }
}

fn apply_old(
    data: &mut HiveData,
    header: &BaseBlock,
    raw_base: Vec<u8>,
    src: &SharedSource,
    dirt: u64,
    report: &mut ReplayReport,
) {
    let hbins = header.hbins_size.min(MAX_HBINS);
    let sectors = (hbins as usize).div_ceil(SECTOR);
    let bitmap_len = sectors.div_ceil(8);
    let Ok(bitmap) = src.read_vec_at(dirt + 4, bitmap_len) else {
        tracing::debug!("old-format log dirty vector truncated");
        return;
    };
    let mut page_at = (dirt + 4 + bitmap_len as u64).next_multiple_of(SECTOR as u64);
    for (byte_i, byte) in bitmap.iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << bit) == 0 {
                continue;
            }
            let sector = byte_i * 8 + bit;
            if sector >= sectors {
                break;
            }
            let Ok(page) = src.read_vec_at(page_at, SECTOR) else {
                tracing::debug!("old-format log ends before its dirty sectors");
                finish_old(data, raw_base, header, hbins);
                return;
            };
            data.patch(BASE_BLOCK_SIZE as u64 + (sector * SECTOR) as u64, &page);
            report.old_format_sectors += 1;
            page_at += SECTOR as u64;
        }
    }
    finish_old(data, raw_base, header, hbins);
}

fn finish_old(data: &mut HiveData, mut raw_base: Vec<u8>, header: &BaseBlock, hbins: u32) {
    data.grow_to(BASE_BLOCK_SIZE as u64 + u64::from(hbins));
    finish_base(&mut raw_base, header.primary_seq, hbins);
    data.set_base(raw_base);
}

/// Marks a base block clean with sequence number `seq` and fixes it up.
fn finish_base(base: &mut [u8], seq: u32, hbins: u32) {
    for at in [4usize, 8] {
        if let Some(d) = base.get_mut(at..at + 4) {
            d.copy_from_slice(&seq.to_le_bytes());
        }
    }
    if let Some(d) = base.get_mut(28..32) {
        d.copy_from_slice(&0u32.to_le_bytes());
    }
    if let Some(d) = base.get_mut(40..44) {
        d.copy_from_slice(&hbins.to_le_bytes());
    }
    fix_checksum(base);
}

/// Marvin32 (the keyed hash Windows uses for `HvLE` entries).
pub(crate) fn marvin32(data: &[u8], seed: u64) -> u64 {
    fn block(lo: &mut u32, hi: &mut u32) {
        *hi ^= *lo;
        *lo = lo.rotate_left(20);
        *lo = lo.wrapping_add(*hi);
        *hi = hi.rotate_left(9);
        *hi ^= *lo;
        *lo = lo.rotate_left(27);
        *lo = lo.wrapping_add(*hi);
        *hi = hi.rotate_left(19);
    }
    let mut lo = seed as u32;
    let mut hi = (seed >> 32) as u32;
    let (chunks, rest) = data.as_chunks::<4>();
    for c in chunks {
        lo = lo.wrapping_add(u32::from_le_bytes(*c));
        block(&mut lo, &mut hi);
    }
    let b = |i: usize| u32::from(rest.get(i).copied().unwrap_or(0));
    let fin = match rest.len() {
        0 => 0x80,
        1 => 0x8000 | b(0),
        2 => 0x80_0000 | b(0) | (b(1) << 8),
        _ => 0x8000_0000 | b(0) | (b(1) << 8) | (b(2) << 16),
    };
    lo = lo.wrapping_add(fin);
    block(&mut lo, &mut hi);
    block(&mut lo, &mut hi);
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Marvin32 with the registry seed (for the log builder).
#[cfg(feature = "builder")]
pub(crate) fn registry_marvin(data: &[u8]) -> u64 {
    marvin32(data, MARVIN_SEED)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference vectors (`SymCrypt` / .NET `Marvin` tests, seed `0x004FB61A001BDBCC`).
    #[test]
    fn marvin32_vectors() {
        let seed = 0x004F_B61A_001B_DBCC;
        assert_eq!(marvin32(&[0xAF], seed), 0x48E7_3FC7_7D75_DDC1);
        assert_eq!(marvin32(&[0xE7, 0x0F], seed), 0xB5F6_E1FC_485D_BFF8);
        assert_eq!(marvin32(&[0x37, 0xF4, 0x95], seed), 0xF0B0_7C78_9B8C_F7E8);
    }
}

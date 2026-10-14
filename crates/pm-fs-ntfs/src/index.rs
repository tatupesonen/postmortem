//! `$I30` directory index parsing (index root, INDX records, slack).

use pm_core::Bytes;

use crate::record::FileName;

/// One entry from a directory index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexEntry {
    pub(crate) mft: u64,
    pub(crate) seq: u16,
    pub(crate) name: FileName,
    /// Found in slack space (deleted), not in the live entry list.
    pub(crate) from_slack: bool,
}

/// Parses the live entries of an index node starting at `node` (the node
/// header), then scans the node's slack for remnants of deleted entries.
pub(crate) fn parse_node(buf: &[u8], node: usize, out: &mut Vec<IndexEntry>) {
    let (Ok(entries_off), Ok(used), Ok(alloc)) =
        (buf.u32_le(node), buf.u32_le(node + 4), buf.u32_le(node + 8))
    else {
        return;
    };
    let start = node + entries_off as usize;
    let used_end = (node + used as usize).min(buf.len());
    let alloc_end = (node + alloc as usize).min(buf.len());
    let mut off = start;
    let mut live_end = off;
    while off + 16 <= used_end {
        let (Ok(mref), Ok(len), Ok(key_len), Ok(flags)) = (
            buf.u64_le(off),
            buf.u16_le(off + 8),
            buf.u16_le(off + 10),
            buf.u16_le(off + 12),
        ) else {
            break;
        };
        let len = usize::from(len);
        if len < 16 || off + len > used_end {
            break;
        }
        if flags & 0x02 != 0 {
            live_end = off + len;
            break; // last entry (no key)
        }
        if key_len >= 66
            && let Some(name) = buf
                .get(off + 16..off + 16 + usize::from(key_len))
                .and_then(FileName::parse)
        {
            out.push(IndexEntry {
                mft: mref & 0x0000_FFFF_FFFF_FFFF,
                seq: (mref >> 48) as u16,
                name,
                from_slack: false,
            });
        }
        off += len;
        live_end = off;
    }
    scan_slack(buf, live_end.max(start), alloc_end, out);
}

/// Scans `[from, to)` at 8-byte alignment for plausible index entries.
fn scan_slack(buf: &[u8], from: usize, to: usize, out: &mut Vec<IndexEntry>) {
    let mut off = from.next_multiple_of(8);
    while off + 16 + 66 <= to {
        match plausible_entry(buf, off, to) {
            Some((entry, len)) => {
                out.push(entry);
                off += len.next_multiple_of(8);
            }
            None => off += 8,
        }
    }
}

/// Validates a candidate index entry in slack space.
fn plausible_entry(buf: &[u8], off: usize, limit: usize) -> Option<(IndexEntry, usize)> {
    let mref = buf.u64_le(off).ok()?;
    let len = usize::from(buf.u16_le(off + 8).ok()?);
    let key_len = usize::from(buf.u16_le(off + 10).ok()?);
    if !(66..=66 + 255 * 2).contains(&key_len) || off + 16 + key_len > limit {
        return None;
    }
    if len != 0 && (len < 16 + key_len || len > 16 + key_len + 16) {
        return None;
    }
    let key = buf.get(off + 16..off + 16 + key_len)?;
    let name = FileName::parse(key)?;
    let name_len = usize::from(key.u8_at(64).ok()?);
    if name_len == 0 || 66 + name_len * 2 > key_len || name.name.contains('\u{fffd}') {
        return None;
    }
    if name
        .name
        .chars()
        .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return None;
    }
    let mft = mref & 0x0000_FFFF_FFFF_FFFF;
    if mft == 0 || mft > 1 << 40 || name.parent == 0 || name.parent > 1 << 40 {
        return None;
    }
    // All four FILETIMEs must be within 1970..2100.
    let plausible_time = |t: u64| (116_444_736_000_000_000..=159_000_000_000_000_000).contains(&t);
    if !name.times.iter().all(|&t| plausible_time(t)) {
        return None;
    }
    let consumed = if len == 0 { 16 + key_len } else { len };
    Some((
        IndexEntry {
            mft,
            seq: (mref >> 48) as u16,
            name,
            from_slack: true,
        },
        consumed,
    ))
}

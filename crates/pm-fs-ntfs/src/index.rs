//! `$I30` directory index parsing (index root, INDX records, slack).
//!
//! Mirrors Autopsy's `ntfs_proc_idxentry` so that recovered names match
//! Autopsy: one pass over the node's whole allocated area, live entries
//! first, then remnants in slack found by stepping 4 bytes at a time. An
//! entry beyond the node's used length (or with a zero key length) is
//! deleted and must pass extra sanity checks; every entry must name this
//! directory as its parent. Stale copies often have their file reference
//! zeroed, so a deleted name may point at MFT entry 0 (shown as such, with
//! no metadata).

use pm_core::Bytes;

use crate::record::FileName;

/// One entry from a directory index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexEntry {
    pub(crate) mft: u64,
    pub(crate) seq: u16,
    pub(crate) name: FileName,
    /// Deleted: found in slack, in an unused INDX block, or with no key.
    pub(crate) from_slack: bool,
}

/// Size of an index entry header (file reference, lengths, flags).
const ENTRY_HEADER: usize = 16;
/// Size of a `$FILE_NAME` value before the name.
const FNAME_HEADER: usize = 66;

/// Where an index node lives and what it belongs to.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NodeCtx {
    /// MFT entry of the directory being listed.
    pub(crate) dir: u64,
    /// Highest valid MFT entry number.
    pub(crate) last_inum: u64,
    /// The whole node is unused (INDX block clear in `$BITMAP`).
    pub(crate) deleted: bool,
}

/// Parses the index node whose header starts at `node`.
pub(crate) fn parse_node(buf: &[u8], node: usize, ctx: NodeCtx, out: &mut Vec<IndexEntry>) {
    let (Ok(entries_off), Ok(used), Ok(alloc)) =
        (buf.u32_le(node), buf.u32_le(node + 4), buf.u32_le(node + 8))
    else {
        return;
    };
    let start = node + entries_off as usize;
    let used_end = (node + used as usize).min(buf.len());
    let end = (node + alloc as usize).min(buf.len()).max(used_end);
    let total = end.saturating_sub(start);
    let mut off = start;
    while off + ENTRY_HEADER + FNAME_HEADER < end {
        off = match entry_at(buf, off, end, used_end, total, ctx) {
            Step::Entry(e, next) => {
                out.push(e);
                next
            }
            Step::Skip(next) => next,
            Step::Reject => off + 4,
        };
    }
}

enum Step {
    Entry(IndexEntry, usize),
    /// A valid entry that is not reported (8.3 DOS name, bad name).
    Skip(usize),
    Reject,
}

fn entry_at(
    buf: &[u8],
    off: usize,
    end: usize,
    used_end: usize,
    total: usize,
    ctx: NodeCtx,
) -> Step {
    let (Ok(mref), Ok(idx_len), Ok(key_len)) =
        (buf.u64_le(off), buf.u16_le(off + 8), buf.u16_le(off + 10))
    else {
        return Step::Reject;
    };
    let (idx_len, key_len) = (usize::from(idx_len), usize::from(key_len));
    let mft = mref & 0x0000_FFFF_FFFF_FFFF;
    if mft > ctx.last_inum || idx_len <= key_len || idx_len % 4 != 0 || idx_len > total {
        return Step::Reject;
    }
    let f = off + ENTRY_HEADER;
    let Ok(parent) = buf.u64_le(f) else {
        return Step::Reject;
    };
    if parent & 0x0000_FFFF_FFFF_FFFF != ctx.dir {
        return Step::Reject;
    }
    let name_len = usize::from(buf.u8_at(f + 64).unwrap_or(0));
    let namespace = buf.u8_at(f + 65).unwrap_or(0xFF);
    let beyond_used = off + idx_len > used_end;
    if key_len == 0 || beyond_used {
        // Extra checks for deleted entries.
        let alloc_size = buf.u64_le(f + 40).unwrap_or(0);
        let real_size = buf.u64_le(f + 48).unwrap_or(u64::MAX);
        let times_ok = [8, 16, 32]
            .iter()
            .all(|&o| buf.u64_le(f + o).is_ok_and(is_time));
        if namespace > 3
            || alloc_size < real_size
            || name_len == 0
            || buf.u8_at(f + FNAME_HEADER).unwrap_or(0) == 0
            || !times_ok
        {
            return Step::Reject;
        }
    }
    let next = if key_len == 0 {
        // A deleted entry's lengths may be cleared: estimate its size.
        (f + FNAME_HEADER + 2 * name_len).next_multiple_of(4)
    } else {
        off + idx_len
    };
    let name_end = f + FNAME_HEADER + 2 * name_len;
    if name_end >= end {
        return Step::Skip(next);
    }
    let Some(name) = buf.get(f..name_end).and_then(FileName::parse) else {
        return Step::Skip(next);
    };
    if name.namespace == crate::record::NS_DOS {
        return Step::Skip(next);
    }
    Step::Entry(
        IndexEntry {
            mft,
            seq: (mref >> 48) as u16,
            name,
            from_slack: ctx.deleted || key_len == 0 || beyond_used,
        },
        next,
    )
}

/// A plausible FILETIME: after 1970 and before 2100. (Autopsy stops at 2020,
/// which would reject entries written by current systems.)
fn is_time(t: u64) -> bool {
    (116_444_736_000_000_000..=159_000_000_000_000_000).contains(&t)
}

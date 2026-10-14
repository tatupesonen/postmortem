//! File allocation table decoding and cluster-chain following.

use pm_core::Bytes;
use pm_fs::FsKind;

/// Decoded meaning of a FAT entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FatEntry {
    Free,
    Next(u32),
    EndOfChain,
    Bad,
}

/// The whole FAT, decoded to 32-bit entries.
#[derive(Debug)]
pub(crate) struct FatTable {
    entries: Vec<u32>,
    kind: FsKind,
}

impl FatTable {
    /// Decodes `raw` (the bytes of one FAT) for `cluster_count + 2` entries.
    pub(crate) fn decode(raw: &[u8], kind: FsKind, cluster_count: u32) -> Self {
        let n = cluster_count as usize + 2;
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let v = match kind {
                FsKind::Fat12 => {
                    let off = i + i / 2;
                    let w = raw.u16_le(off).unwrap_or(0);
                    u32::from(if i % 2 == 1 { w >> 4 } else { w & 0x0FFF })
                }
                FsKind::Fat16 => u32::from(raw.u16_le(i * 2).unwrap_or(0)),
                _ => raw.u32_le(i * 4).unwrap_or(0) & 0x0FFF_FFFF,
            };
            entries.push(v);
        }
        Self { entries, kind }
    }

    pub(crate) fn get(&self, cluster: u32) -> FatEntry {
        let Some(&v) = self.entries.get(cluster as usize) else {
            return FatEntry::Bad;
        };
        let (bad, eoc) = match self.kind {
            FsKind::Fat12 => (0xFF7, 0xFF8),
            FsKind::Fat16 => (0xFFF7, 0xFFF8),
            _ => (0x0FFF_FFF7, 0x0FFF_FFF8),
        };
        match v {
            0 => FatEntry::Free,
            v if v == bad => FatEntry::Bad,
            v if v >= eoc => FatEntry::EndOfChain,
            // 1 and other reserved values behave as a broken chain.
            1 => FatEntry::Bad,
            v => FatEntry::Next(v),
        }
    }

    pub(crate) fn is_free(&self, cluster: u32) -> bool {
        self.get(cluster) == FatEntry::Free
    }

    /// Follows the chain from `start`. Stops at end-of-chain, free/bad
    /// entries, invalid clusters, loops, or after `max` clusters.
    pub(crate) fn chain(&self, start: u32, max: usize) -> Vec<u32> {
        let mut out = Vec::new();
        let limit = self.entries.len();
        let mut seen = vec![false; limit];
        let mut c = start;
        while out.len() < max {
            if c < 2 || c as usize >= limit || seen.get(c as usize).copied().unwrap_or(true) {
                break;
            }
            if let Some(s) = seen.get_mut(c as usize) {
                *s = true;
            }
            out.push(c);
            match self.get(c) {
                FatEntry::Next(n) => c = n,
                FatEntry::EndOfChain | FatEntry::Free | FatEntry::Bad => break,
            }
        }
        out
    }

    /// Free clusters as ascending, coalesced `(first_cluster, count)` runs.
    pub(crate) fn free_runs(&self) -> Vec<(u32, u32)> {
        let mut runs = Vec::new();
        let mut cur: Option<(u32, u32)> = None;
        for c in 2..self.entries.len() as u32 {
            if self.is_free(c) {
                cur = Some(match cur {
                    Some((s, n)) if s + n == c => (s, n + 1),
                    Some(r) => {
                        runs.push(r);
                        (c, 1)
                    }
                    None => (c, 1),
                });
            }
        }
        runs.extend(cur);
        runs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fat12_packing() {
        // Entries: [0]=0xFF8 [1]=0xFFF [2]=0x003 [3]=0xFFF [4]=0x000 [5]=0x007
        let raw = [0xF8, 0xFF, 0xFF, 0x03, 0xF0, 0xFF, 0x00, 0x70, 0x00];
        let t = FatTable::decode(&raw, FsKind::Fat12, 4);
        assert_eq!(t.get(2), FatEntry::Next(3));
        assert_eq!(t.get(3), FatEntry::EndOfChain);
        assert_eq!(t.get(4), FatEntry::Free);
        assert_eq!(t.get(5), FatEntry::Next(7));
        assert_eq!(t.chain(2, 100), vec![2, 3]);
    }

    #[test]
    fn chain_loop_and_limit() {
        let mut raw = vec![0u8; 4 * 10];
        let set = |raw: &mut Vec<u8>, i: usize, v: u32| {
            raw[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        };
        set(&mut raw, 2, 3);
        set(&mut raw, 3, 4);
        set(&mut raw, 4, 2); // loop
        let t = FatTable::decode(&raw, FsKind::Fat32, 8);
        assert_eq!(t.chain(2, 100), vec![2, 3, 4]);
        assert_eq!(t.chain(2, 2), vec![2, 3]);
        assert_eq!(t.chain(0, 10), Vec::<u32>::new());
        assert_eq!(t.chain(99, 10), Vec::<u32>::new());
    }

    #[test]
    fn free_runs_coalesce() {
        let mut raw = vec![0u8; 2 * 12];
        for i in [2usize, 3, 7] {
            raw[i * 2..i * 2 + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        }
        let t = FatTable::decode(&raw, FsKind::Fat16, 10);
        assert_eq!(t.free_runs(), vec![(4, 3), (8, 4)]);
    }

    #[test]
    fn bad_cluster_marker() {
        let raw = [0, 0, 0, 0, 0xF7, 0xFF];
        let t = FatTable::decode(&raw, FsKind::Fat16, 1);
        assert_eq!(t.get(2), FatEntry::Bad);
        assert_eq!(t.get(1000), FatEntry::Bad);
    }
}

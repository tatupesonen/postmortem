//! OLE2 compound files ("CFB", Microsoft's Compound File Binary format).
//!
//! [`Cfb::open`] validates a file the way Apache POI 5.3's
//! `POIFSFileSystem` does when Tika opens it (header, sector size, FAT and
//! DIFAT chains with POI's loop detection, the directory stream, the
//! red-black sibling tree with duplicate-name checks, the mini-FAT chain),
//! so a file POI rejects is rejected here too. Streams can then be read
//! with chain-loop and bounds protection.

use std::collections::{HashMap, HashSet};

use pm_core::ReadAt;

/// The OLE2 signature.
pub const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

const END_OF_CHAIN: i32 = -2;
const UNUSED: i32 = -1;
const MINI_CUTOFF: u32 = 4096;
const MINI_SECTOR: u64 = 64;

/// Why a compound file cannot be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CfbError {
    #[error("not an OLE2 compound file")]
    NotOle2,
    #[error("invalid compound file: {0}")]
    Invalid(&'static str),
    #[error("compound file too large to open")]
    TooLarge,
}

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Index in the directory stream.
    pub id: usize,
    pub name: String,
    /// 1 storage, 2 stream, 5 root.
    pub kind: u8,
    pub start: i32,
    pub size: u32,
    prev: i32,
    next: i32,
    child: i32,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == 1 || self.kind == 5
    }
}

/// An opened compound file.
#[derive(Debug)]
pub struct Cfb<'a> {
    src: &'a dyn ReadAt,
    size: u64,
    sector: u64,
    fat: Vec<i32>,
    minifat: Vec<i32>,
    entries: Vec<Option<Entry>>,
    /// Children of each directory entry (by entry id), in POI's order.
    children: HashMap<usize, Vec<usize>>,
}

fn le32(b: &[u8], o: usize) -> i32 {
    b.get(o..o + 4)
        .and_then(|s| s.try_into().ok())
        .map_or(0, i32::from_le_bytes)
}

fn le16(b: &[u8], o: usize) -> i16 {
    b.get(o..o + 2)
        .and_then(|s| s.try_into().ok())
        .map_or(0, i16::from_le_bytes)
}

/// POI's `ChainLoopDetector`.
struct Loop {
    used: Vec<bool>,
}

impl Loop {
    fn new(raw_size: u64, block: u64) -> Self {
        let n = raw_size.div_ceil(block.max(1));
        Self {
            used: vec![false; usize::try_from(n).unwrap_or(0)],
        }
    }

    fn claim(&mut self, i: i32) -> Result<(), CfbError> {
        let i = usize::try_from(i).map_err(|_| CfbError::Invalid("negative block"))?;
        if let Some(u) = self.used.get_mut(i) {
            if *u {
                return Err(CfbError::Invalid("block chain loop"));
            }
            *u = true;
        }
        Ok(())
    }
}

impl<'a> Cfb<'a> {
    /// Largest file opened (memory for the FAT is ~1/128 of the size).
    pub const MAX_SIZE: u64 = 4 * 1024 * 1024 * 1024;

    /// Reads a big block (zero-padded at the end of the file).
    fn block(&self, i: i32) -> Result<Vec<u8>, CfbError> {
        let i = u64::try_from(i).map_err(|_| CfbError::Invalid("negative block"))?;
        let at = (i + 1) * self.sector;
        if at >= self.size {
            return Err(CfbError::Invalid("block past the end"));
        }
        let mut b = vec![0u8; usize::try_from(self.sector).unwrap_or(512)];
        let _ = self.src.read_at(at, &mut b);
        Ok(b)
    }

    fn next_in(table: &[i32], i: i32) -> Result<i32, CfbError> {
        usize::try_from(i)
            .ok()
            .and_then(|i| table.get(i).copied())
            .ok_or(CfbError::Invalid("block outside the allocation table"))
    }

    /// Validates and opens a compound file like POI.
    pub fn open(src: &'a dyn ReadAt) -> Result<Self, CfbError> {
        let size = src.size();
        let mut hdr = [0u8; 512];
        if size < 512 || src.read_at(0, &mut hdr).unwrap_or(0) < 512 {
            return Err(CfbError::NotOle2);
        }
        if hdr[..8] != SIGNATURE {
            return Err(CfbError::NotOle2);
        }
        if size > Self::MAX_SIZE {
            return Err(CfbError::TooLarge);
        }
        let sector: u64 = match hdr[30] {
            9 => 512,
            12 => 4096,
            _ => return Err(CfbError::Invalid("sector size")),
        };
        let bat_count = le32(&hdr, 0x2C);
        let prop_start = le32(&hdr, 0x30);
        let sbat_start = le32(&hdr, 0x3C);
        let sbat_count = le32(&hdr, 0x40);
        let xbat_start = le32(&hdr, 0x44);
        let xbat_count = le32(&hdr, 0x48);
        if bat_count < 0 {
            return Err(CfbError::Invalid("FAT count"));
        }
        let mut cfb = Self {
            src,
            size,
            sector,
            fat: Vec::new(),
            minifat: Vec::new(),
            entries: Vec::new(),
            children: HashMap::new(),
        };
        let per_block = usize::try_from(sector / 4).unwrap_or(128);
        let mut lp = Loop::new(size, sector);
        let in_header = usize::try_from(bat_count).unwrap_or(0).min(109);
        let read_bat = |cfb: &mut Self, lp: &mut Loop, at: i32| -> Result<(), CfbError> {
            lp.claim(at)?;
            let b = cfb.block(at)?;
            cfb.fat.extend((0..per_block).map(|k| le32(&b, k * 4)));
            Ok(())
        };
        for k in 0..in_header {
            read_bat(&mut cfb, &mut lp, le32(&hdr, 0x4C + k * 4))?;
        }
        let mut remaining = i64::from(bat_count) - in_header as i64;
        let mut next = xbat_start;
        for _ in 0..xbat_count.max(0) {
            lp.claim(next)?;
            let x = cfb.block(next)?;
            next = le32(&x, (per_block - 1) * 4);
            let n = remaining.min(per_block as i64 - 1);
            for j in 0..usize::try_from(n.max(0)).unwrap_or(0) {
                let at = le32(&x, j * 4);
                if at == UNUSED || at == END_OF_CHAIN {
                    break;
                }
                read_bat(&mut cfb, &mut lp, at)?;
            }
            remaining -= n;
        }
        // The directory stream.
        let dir = cfb.chain_big(prop_start, None)?;
        for rec in dir.chunks_exact(128) {
            let kind = rec.get(0x42).copied().unwrap_or(0);
            if !matches!(kind, 1 | 2 | 5) {
                cfb.entries.push(None);
                continue;
            }
            let name_size = le16(rec, 0x40);
            let name_len = i32::from(name_size) / 2 - 1;
            if name_len > 64 {
                return Err(CfbError::Invalid("entry name length"));
            }
            let units: Vec<u16> = (0..usize::try_from(name_len.max(0)).unwrap_or(0))
                .map(|j| le16(rec, j * 2) as u16)
                .collect();
            cfb.entries.push(Some(Entry {
                id: cfb.entries.len(),
                name: String::from_utf16_lossy(&units),
                kind,
                start: le32(rec, 0x74),
                size: le32(rec, 0x78) as u32,
                prev: le32(rec, 0x44),
                next: le32(rec, 0x48),
                child: le32(rec, 0x4C),
            }));
        }
        match cfb.entries.first() {
            None => return Err(CfbError::Invalid("empty directory")),
            Some(Some(e)) if e.kind == 2 => return Err(CfbError::Invalid("root is a stream")),
            Some(Some(_)) => cfb.populate()?,
            Some(None) => {}
        }
        if cfb
            .entries
            .first()
            .is_none_or(|e| e.as_ref().is_none_or(|e| e.kind != 5))
        {
            // `PropertyTable.getRoot()`: not a RootProperty.
            return Err(CfbError::Invalid("no root entry"));
        }
        // The mini FAT.
        let mut next = sbat_start;
        let mut i = 0;
        while i < sbat_count && next != END_OF_CHAIN {
            lp.claim(next)?;
            let b = cfb.block(next)?;
            cfb.minifat.extend((0..per_block).map(|k| le32(&b, k * 4)));
            next = Self::next_in(&cfb.fat, next)?;
            i += 1;
        }
        Ok(cfb)
    }

    /// `PropertyTable.populatePropertyTree` from the root.
    fn populate(&mut self) -> Result<(), CfbError> {
        let n = self.entries.len();
        let mut names: HashMap<usize, HashSet<String>> = HashMap::new();
        let mut dirs: Vec<usize> = vec![0];
        let mut budget = n.saturating_mul(4) + 16;
        while let Some(d) = dirs.pop() {
            let Some(Some(dir)) = self.entries.get(d) else {
                continue;
            };
            let child = dir.child;
            if child == UNUSED {
                continue;
            }
            let first = usize::try_from(child)
                .ok()
                .filter(|&c| c < n)
                .ok_or(CfbError::Invalid("child index"))?;
            let mut stack = vec![first];
            while let Some(p) = stack.pop() {
                budget = budget
                    .checked_sub(1)
                    .ok_or(CfbError::Invalid("directory tree"))?;
                let Some(Some(e)) = self.entries.get(p) else {
                    continue;
                };
                let (name, is_dir, prev, next) = (e.name.clone(), e.is_dir(), e.prev, e.next);
                if !names.entry(d).or_default().insert(name) {
                    return Err(CfbError::Invalid("duplicate name"));
                }
                self.children.entry(d).or_default().push(p);
                if is_dir {
                    dirs.push(p);
                }
                for idx in [prev, next] {
                    if let Some(i) = usize::try_from(idx)
                        .ok()
                        .filter(|&i| idx != UNUSED && i < n)
                    {
                        stack.push(i);
                    }
                }
            }
        }
        Ok(())
    }

    /// Reads a big-block chain (`POIFSStream`): every block until
    /// end-of-chain, with its own loop detector.
    fn chain_big(&self, start: i32, limit: Option<u64>) -> Result<Vec<u8>, CfbError> {
        let mut lp = Loop::new(self.size, self.sector);
        let mut out = Vec::new();
        let mut cur = start;
        while cur != END_OF_CHAIN {
            if limit.is_some_and(|l| out.len() as u64 >= l) {
                break;
            }
            lp.claim(cur)?;
            out.extend_from_slice(&self.block(cur)?);
            cur = Self::next_in(&self.fat, cur)?;
        }
        Ok(out)
    }

    /// The root entry.
    pub fn root(&self) -> Option<&Entry> {
        self.entries.first().and_then(Option::as_ref)
    }

    /// Children of a storage, in POI's order.
    pub fn children(&self, dir: &Entry) -> Vec<&Entry> {
        self.children
            .get(&dir.id)
            .map(|c| {
                c.iter()
                    .filter_map(|&i| self.entries.get(i).and_then(Option::as_ref))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Names of the root's children.
    pub fn top_level_names(&self) -> Vec<String> {
        self.root()
            .map(|r| self.children(r).iter().map(|e| e.name.clone()).collect())
            .unwrap_or_default()
    }

    /// A child of `dir` whose upper-cased name equals `upper`
    /// (`OfficeParser.getUCEntry`).
    pub fn child_uc(&self, dir: &Entry, upper: &str) -> Option<&Entry> {
        self.children(dir)
            .into_iter()
            .find(|e| e.name.to_uppercase() == upper)
    }

    /// A child of the root by exact name.
    pub fn root_child(&self, name: &str) -> Option<&Entry> {
        let r = self.root()?;
        self.children(r).into_iter().find(|e| e.name == name)
    }

    /// Reads up to `max` bytes of a stream.
    pub fn read_stream(&self, e: &Entry, max: usize) -> Result<Vec<u8>, CfbError> {
        if e.kind != 2 {
            return Err(CfbError::Invalid("not a stream"));
        }
        let want = usize::try_from(e.size).unwrap_or(usize::MAX).min(max);
        let mut data = if e.size < MINI_CUTOFF {
            self.read_mini(e.start, want)?
        } else {
            self.chain_big(e.start, Some(want as u64))?
        };
        if data.len() < want {
            return Err(CfbError::Invalid("stream shorter than its size"));
        }
        data.truncate(want);
        Ok(data)
    }

    /// Reads up to `len` bytes of a stream from `offset` (fewer at its
    /// end), without copying the sectors before it.
    pub fn read_stream_range(
        &self,
        e: &Entry,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, CfbError> {
        if e.kind != 2 {
            return Err(CfbError::Invalid("not a stream"));
        }
        let size = u64::from(e.size);
        if offset >= size || len == 0 {
            return Ok(Vec::new());
        }
        let want = usize::try_from((size - offset).min(len as u64)).unwrap_or(len);
        if e.size < MINI_CUTOFF {
            let all = self.read_stream(e, usize::MAX)?;
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            return Ok(all
                .get(start..start.saturating_add(want))
                .unwrap_or_default()
                .to_vec());
        }
        let skip_blocks = offset / self.sector;
        let mut in_block = usize::try_from(offset % self.sector).unwrap_or(0);
        let mut lp = Loop::new(self.size, self.sector);
        let mut cur = e.start;
        for _ in 0..skip_blocks {
            if cur == END_OF_CHAIN {
                return Err(CfbError::Invalid("stream shorter than its size"));
            }
            lp.claim(cur)?;
            cur = Self::next_in(&self.fat, cur)?;
        }
        let mut out = Vec::with_capacity(want);
        while out.len() < want {
            if cur == END_OF_CHAIN {
                return Err(CfbError::Invalid("stream shorter than its size"));
            }
            lp.claim(cur)?;
            let b = self.block(cur)?;
            let part = b.get(in_block..).unwrap_or_default();
            let take = part.len().min(want - out.len());
            out.extend_from_slice(part.get(..take).unwrap_or_default());
            in_block = 0;
            cur = Self::next_in(&self.fat, cur)?;
        }
        Ok(out)
    }

    /// Reads a mini-stream chain.
    fn read_mini(&self, start: i32, want: usize) -> Result<Vec<u8>, CfbError> {
        let root = self.root().ok_or(CfbError::Invalid("no root entry"))?;
        let mini_len = u64::from(root.size);
        // The mini stream itself lives in big blocks.
        let mini = self.chain_big(root.start, Some(mini_len))?;
        let mut lp = Loop::new(mini_len, MINI_SECTOR);
        let mut out = Vec::new();
        let mut cur = start;
        while cur != END_OF_CHAIN && out.len() < want {
            lp.claim(cur)?;
            let at = usize::try_from(cur).map_err(|_| CfbError::Invalid("mini block"))?
                * MINI_SECTOR as usize;
            let b = mini
                .get(at..at + MINI_SECTOR as usize)
                .ok_or(CfbError::Invalid("mini block past the end"))?;
            out.extend_from_slice(b);
            cur = Self::next_in(&self.minifat, cur)?;
        }
        Ok(out)
    }

    /// Finds an entry anywhere below `dir` (depth-limited search by
    /// upper-case name, `POIFSContainerDetector.findRecursively`).
    pub fn find_recursively(&self, dir: &Entry, upper: &str, max_depth: usize) -> bool {
        fn walk(c: &Cfb<'_>, e: &Entry, upper: &str, depth: usize, max: usize) -> bool {
            if e.name.to_uppercase() == upper {
                return true;
            }
            if depth >= max {
                return false;
            }
            e.is_dir()
                && c.children(e)
                    .iter()
                    .any(|ch| walk(c, ch, upper, depth + 1, max))
        }
        walk(self, dir, upper, 0, max_depth)
    }
}

/// Builds compound files for tests and fixtures (512-byte sectors, every
/// stream in big blocks unless it is smaller than 4096 bytes).
#[cfg(any(test, feature = "builder"))]
#[allow(
    clippy::indexing_slicing,
    clippy::missing_panics_doc,
    clippy::single_match_else,
    reason = "fixture builder with fixed layouts"
)]
pub mod builder {
    /// A stream: path components and data.
    #[derive(Debug, Clone)]
    pub struct Stream {
        pub path: Vec<String>,
        pub data: Vec<u8>,
    }

    impl Stream {
        /// A stream in the root storage.
        pub fn root(name: &str, data: &[u8]) -> Self {
            Self {
                path: vec![name.to_owned()],
                data: data.to_vec(),
            }
        }
    }

    struct Node {
        name: String,
        kind: u8,
        data: Vec<u8>,
        children: Vec<usize>,
        start: i32,
        size: u32,
        left: i32,
        right: i32,
        child: i32,
    }

    fn put32(v: &mut [u8], o: usize, x: i32) {
        v[o..o + 4].copy_from_slice(&x.to_le_bytes());
    }

    /// Builds a compound file containing `streams` (storages are created
    /// from the paths).
    pub fn build(streams: &[Stream]) -> Vec<u8> {
        let mut nodes = vec![Node {
            name: "Root Entry".into(),
            kind: 5,
            data: Vec::new(),
            children: Vec::new(),
            start: -2,
            size: 0,
            left: -1,
            right: -1,
            child: -1,
        }];
        for s in streams {
            let mut parent = 0;
            for (i, part) in s.path.iter().enumerate() {
                let last = i + 1 == s.path.len();
                let found = nodes[parent]
                    .children
                    .iter()
                    .copied()
                    .find(|&c| nodes[c].name == *part);
                let id = match found {
                    Some(c) => c,
                    None => {
                        nodes.push(Node {
                            name: part.clone(),
                            kind: if last { 2 } else { 1 },
                            data: if last { s.data.clone() } else { Vec::new() },
                            children: Vec::new(),
                            start: -2,
                            size: 0,
                            left: -1,
                            right: -1,
                            child: -1,
                        });
                        let id = nodes.len() - 1;
                        nodes[parent].children.push(id);
                        id
                    }
                };
                parent = id;
            }
        }
        // Siblings as a right-leaning list (a valid, if unbalanced, tree).
        for p in 0..nodes.len() {
            let ch = nodes[p].children.clone();
            if let Some(&first) = ch.first() {
                nodes[p].child = first as i32;
            }
            for w in ch.windows(2) {
                nodes[w[0]].right = w[1] as i32;
            }
        }
        // Mini stream for small streams, big blocks for the rest.
        let mut mini = Vec::new();
        let mut minifat: Vec<i32> = Vec::new();
        let mut big: Vec<(usize, Vec<u8>)> = Vec::new();
        for (i, n) in nodes.iter_mut().enumerate() {
            if n.kind != 2 {
                continue;
            }
            n.size = n.data.len() as u32;
            if n.data.is_empty() {
                n.start = -2;
            } else if n.data.len() < 4096 {
                let first = minifat.len();
                let blocks = n.data.len().div_ceil(64);
                for k in 0..blocks {
                    minifat.push(if k + 1 == blocks {
                        -2
                    } else {
                        (first + k + 1) as i32
                    });
                }
                n.start = first as i32;
                let mut d = n.data.clone();
                d.resize(blocks * 64, 0);
                mini.extend(d);
            } else {
                big.push((i, n.data.clone()));
            }
        }
        // Sector layout: [FAT][dir][minifat][mini stream][big streams].
        let mut sectors: Vec<Vec<u8>> = Vec::new();
        let mut fat: Vec<i32> = Vec::new();
        let alloc = |data: &[u8], sectors: &mut Vec<Vec<u8>>, fat: &mut Vec<i32>| -> i32 {
            if data.is_empty() {
                return -2;
            }
            let first = sectors.len();
            let n = data.len().div_ceil(512);
            for k in 0..n {
                let mut s = data[k * 512..((k + 1) * 512).min(data.len())].to_vec();
                s.resize(512, 0);
                sectors.push(s);
                fat.push(if k + 1 == n {
                    -2
                } else {
                    (first + k + 1) as i32
                });
            }
            first as i32
        };
        // Reserve FAT sectors first; size them generously.
        let dir_len = nodes.len().div_ceil(4) * 512;
        let total_guess = 8
            + dir_len / 512
            + minifat.len() / 128
            + mini.len() / 512
            + big.iter().map(|(_, d)| d.len() / 512 + 1).sum::<usize>();
        let fat_sectors = (total_guess + 2).div_ceil(127).max(1);
        for _ in 0..fat_sectors {
            sectors.push(vec![0; 512]);
            fat.push(-3);
        }
        let dir_start_placeholder = vec![0u8; dir_len];
        let dir_start = alloc(&dir_start_placeholder, &mut sectors, &mut fat);
        let mut mf = Vec::new();
        for x in &minifat {
            mf.extend_from_slice(&x.to_le_bytes());
        }
        let minifat_start = alloc(&mf, &mut sectors, &mut fat);
        let mini_start = alloc(&mini, &mut sectors, &mut fat);
        nodes[0].start = mini_start;
        nodes[0].size = mini.len() as u32;
        for (i, d) in &big {
            nodes[*i].start = alloc(d, &mut sectors, &mut fat);
        }
        // Directory.
        let mut dir = vec![0u8; dir_len];
        for (i, n) in nodes.iter().enumerate() {
            let o = i * 128;
            let units: Vec<u16> = n.name.encode_utf16().collect();
            for (k, u) in units.iter().take(31).enumerate() {
                dir[o + k * 2..o + k * 2 + 2].copy_from_slice(&u.to_le_bytes());
            }
            let ns = ((units.len().min(31) + 1) * 2) as u16;
            dir[o + 0x40..o + 0x42].copy_from_slice(&ns.to_le_bytes());
            dir[o + 0x42] = n.kind;
            dir[o + 0x43] = 1;
            put32(&mut dir, o + 0x44, n.left);
            put32(&mut dir, o + 0x48, n.right);
            put32(&mut dir, o + 0x4C, n.child);
            put32(&mut dir, o + 0x74, n.start);
            put32(&mut dir, o + 0x78, n.size as i32);
        }
        for k in 0..dir_len / 512 {
            sectors[dir_start as usize + k].copy_from_slice(&dir[k * 512..(k + 1) * 512]);
        }
        // FAT.
        assert!(fat.len() <= fat_sectors * 128, "builder FAT too small");
        let mut fat_bytes = Vec::new();
        for k in 0..fat_sectors * 128 {
            fat_bytes.extend_from_slice(&fat.get(k).copied().unwrap_or(-1).to_le_bytes());
        }
        for k in 0..fat_sectors {
            sectors[k].copy_from_slice(&fat_bytes[k * 512..(k + 1) * 512]);
        }
        // Header.
        let mut h = vec![0u8; 512];
        h[..8].copy_from_slice(&super::SIGNATURE);
        h[0x18..0x1A].copy_from_slice(&0x3Eu16.to_le_bytes());
        h[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
        h[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
        h[0x1E] = 9;
        h[0x20] = 6;
        put32(&mut h, 0x2C, fat_sectors as i32);
        put32(&mut h, 0x30, dir_start);
        put32(&mut h, 0x38, 4096);
        put32(&mut h, 0x3C, minifat_start);
        put32(&mut h, 0x40, mf.len().div_ceil(512) as i32);
        put32(&mut h, 0x44, -2);
        put32(&mut h, 0x48, 0);
        for k in 0..109 {
            put32(
                &mut h,
                0x4C + k * 4,
                if k < fat_sectors { k as i32 } else { -1 },
            );
        }
        let mut out = h;
        for s in sectors {
            out.extend(s);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::builder::{Stream, build};
    use super::*;
    use pm_core::io::MemSource;

    #[test]
    fn round_trip() {
        let big = vec![7u8; 9000];
        let file = build(&[
            Stream::root("WordDocument", b"hello"),
            Stream::root("Big", &big),
            Stream {
                path: vec!["Dir".into(), "Inner".into()],
                data: b"x".to_vec(),
            },
        ]);
        let src = MemSource::new(file);
        let c = Cfb::open(&src).unwrap();
        let mut names = c.top_level_names();
        names.sort();
        assert_eq!(names, ["Big", "Dir", "WordDocument"]);
        let w = c.root_child("WordDocument").unwrap();
        assert_eq!(c.read_stream(w, 100).unwrap(), b"hello");
        let b = c.root_child("Big").unwrap();
        assert_eq!(c.read_stream(b, usize::MAX).unwrap(), big);
        let root = c.root().unwrap().clone();
        assert!(c.find_recursively(&root, "INNER", 10));
        assert!(!c.find_recursively(&root, "INNER", 1));
    }

    #[test]
    fn ranged_reads() {
        let big: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let file = build(&[
            Stream::root("Big", &big),
            Stream::root("Small", b"0123456789"),
        ]);
        let src = MemSource::new(file);
        let c = Cfb::open(&src).unwrap();
        let b = c.root_child("Big").unwrap();
        for (off, len) in [
            (0u64, 10usize),
            (511, 3),
            (512, 600),
            (19_990, 50),
            (5000, 0),
        ] {
            let want = big
                .get(off as usize..(off as usize + len).min(big.len()))
                .unwrap();
            assert_eq!(
                c.read_stream_range(b, off, len).unwrap(),
                want,
                "{off}+{len}"
            );
        }
        assert!(c.read_stream_range(b, 20_000, 5).unwrap().is_empty());
        let s = c.root_child("Small").unwrap();
        assert_eq!(c.read_stream_range(s, 3, 4).unwrap(), b"3456");
        assert_eq!(c.read_stream_range(s, 8, 40).unwrap(), b"89");
    }

    #[test]
    fn rejects() {
        let mut file = build(&[Stream::root("A", b"1"), Stream::root("B", b"2")]);
        // Unsupported sector shift.
        let mut bad = file.clone();
        bad[30] = 10;
        assert!(Cfb::open(&MemSource::new(bad)).is_err());
        // Duplicate names.
        let dir_off = 512 * (1 + le32(&file, 0x30) as usize);
        file[dir_off + 128 * 2] = b'A';
        assert_eq!(
            Cfb::open(&MemSource::new(file.clone())).unwrap_err(),
            CfbError::Invalid("duplicate name")
        );
        // Unknown root type.
        file[dir_off + 0x42] = 7;
        assert_eq!(
            Cfb::open(&MemSource::new(file)).unwrap_err(),
            CfbError::Invalid("no root entry")
        );
        assert_eq!(
            Cfb::open(&MemSource::new(vec![0u8; 600])).unwrap_err(),
            CfbError::NotOle2
        );
    }
}

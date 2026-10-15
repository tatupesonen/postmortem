//! A small, bounds-checked reader for OLE compound files (MS-CFB), enough
//! for jump lists: header, DIFAT/FAT, mini FAT, directory and streams.

use std::collections::HashSet;

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;

use crate::{Error, Result};

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// Sector numbers at or above this are markers (end of chain, free, …).
const MAX_REG_SECT: u32 = 0xFFFF_FFFA;
const NO_STREAM: u32 = 0xFFFF_FFFF;
/// Most directory entries read.
const MAX_ENTRIES: usize = 1 << 16;

/// Directory entry object types.
pub const STORAGE: u8 = 1;
/// A stream entry.
pub const STREAM: u8 = 2;
/// The root storage.
pub const ROOT: u8 = 5;

/// One directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: u8,
    pub left: u32,
    pub right: u32,
    pub child: u32,
    pub start: u32,
    pub size: u64,
    /// FILETIMEs (0 for streams).
    pub created: u64,
    pub modified: u64,
}

/// An opened compound file.
#[derive(Debug)]
pub struct CompoundFile<'a> {
    data: &'a [u8],
    sector_size: usize,
    mini_size: usize,
    cutoff: u64,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    dir: Vec<DirEntry>,
    mini_stream: Vec<u8>,
}

impl<'a> CompoundFile<'a> {
    /// Parses the header, allocation tables and directory.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.get(..8) != Some(&SIGNATURE[..]) {
            return Err(Error::NotCompoundFile);
        }
        let major = data.u16_le(26)?;
        let shift = data.u16_le(30)?;
        if !(7..=16).contains(&shift) {
            return Err(Error::Malformed("sector shift"));
        }
        let sector_size = 1usize << shift;
        let mini_shift = data.u16_le(32)?.min(12);
        let mut cf = Self {
            data,
            sector_size,
            mini_size: 1usize << mini_shift,
            cutoff: u64::from(data.u32_le(56)?),
            fat: Vec::new(),
            minifat: Vec::new(),
            dir: Vec::new(),
            mini_stream: Vec::new(),
        };
        let max_sectors = data.len() / sector_size + 1;
        // DIFAT: 109 entries in the header, then a chain of DIFAT sectors.
        let mut fat_sectors: Vec<u32> = data
            .get(76..512)
            .unwrap_or_default()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .filter(|&s| s < MAX_REG_SECT)
            .collect();
        let mut difat = data.u32_le(68)?;
        let mut seen = HashSet::new();
        while difat < MAX_REG_SECT && seen.insert(difat) && seen.len() <= max_sectors {
            let s = cf.sector(difat)?;
            let entries: Vec<u32> = s
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect();
            let (next, body) = entries.split_last().ok_or(Error::Malformed("DIFAT"))?;
            fat_sectors.extend(body.iter().copied().filter(|&s| s < MAX_REG_SECT));
            difat = *next;
        }
        fat_sectors.truncate(max_sectors);
        for s in fat_sectors {
            let sec = cf.sector(s)?;
            cf.fat.extend(
                sec.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_le_bytes(*c)),
            );
        }
        // Directory.
        let dir_bytes = cf.chain_bytes(data.u32_le(48)?, None)?;
        for e in dir_bytes.as_chunks::<128>().0.iter().take(MAX_ENTRIES) {
            let name_len = usize::from(e.u16_le(64)?).min(64);
            let size = e.u64_le(120)?;
            cf.dir.push(DirEntry {
                name: utf16le_lossy(e.get(..name_len).unwrap_or_default()),
                kind: e.u8_at(66)?,
                left: e.u32_le(68)?,
                right: e.u32_le(72)?,
                child: e.u32_le(76)?,
                created: e.u64_le(100)?,
                modified: e.u64_le(108)?,
                start: e.u32_le(116)?,
                size: if major == 3 { size & 0xFFFF_FFFF } else { size },
            });
        }
        let root = cf
            .dir
            .first()
            .cloned()
            .ok_or(Error::Malformed("no root entry"))?;
        if root.kind != ROOT {
            return Err(Error::Malformed("first directory entry is not the root"));
        }
        // Mini FAT and mini stream.
        let mf = cf.chain_bytes(data.u32_le(60)?, None)?;
        cf.minifat = mf
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        cf.mini_stream = cf.chain_bytes(root.start, Some(root.size))?;
        Ok(cf)
    }

    fn sector(&self, s: u32) -> Result<&'a [u8]> {
        let off = (s as usize)
            .checked_add(1)
            .and_then(|n| n.checked_mul(self.sector_size))
            .ok_or(Error::Malformed("sector number"))?;
        self.data
            .get(off..off + self.sector_size)
            .ok_or(Error::Malformed("sector outside the file"))
    }

    /// Bytes of a regular-sector chain, cut to `size` when given.
    fn chain_bytes(&self, start: u32, size: Option<u64>) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut s = start;
        let mut steps = 0usize;
        // A chain longer than the file has sectors is a loop.
        let limit = self.data.len() / self.sector_size + 1;
        while s < MAX_REG_SECT {
            if steps > limit || size.is_some_and(|n| out.len() as u64 >= n) {
                break;
            }
            out.extend_from_slice(self.sector(s)?);
            s = self.fat.get(s as usize).copied().unwrap_or(NO_STREAM);
            steps += 1;
        }
        if let Some(n) = size {
            out.truncate(usize::try_from(n).unwrap_or(usize::MAX));
        }
        Ok(out)
    }

    fn mini_chain_bytes(&self, start: u32, size: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let mut s = start;
        let mut steps = 0usize;
        let limit = self.mini_stream.len() / self.mini_size + 1;
        while s < MAX_REG_SECT && steps <= limit && (out.len() as u64) < size {
            let off = s as usize * self.mini_size;
            match self.mini_stream.get(off..off + self.mini_size) {
                Some(b) => out.extend_from_slice(b),
                None => break,
            }
            s = self.minifat.get(s as usize).copied().unwrap_or(NO_STREAM);
            steps += 1;
        }
        out.truncate(usize::try_from(size).unwrap_or(usize::MAX));
        out
    }

    /// All directory entries (index = entry id).
    pub fn entries(&self) -> &[DirEntry] {
        &self.dir
    }

    /// Streams directly under the root storage (tree order).
    pub fn root_streams(&self) -> Vec<&DirEntry> {
        let mut out = Vec::new();
        let Some(root) = self.dir.first() else {
            return out;
        };
        let mut stack = vec![root.child];
        let mut seen = HashSet::new();
        while let Some(id) = stack.pop() {
            if id == NO_STREAM || !seen.insert(id) {
                continue;
            }
            let Some(e) = self.dir.get(id as usize) else {
                continue;
            };
            if e.kind == STREAM {
                out.push(e);
            }
            stack.push(e.right);
            stack.push(e.left);
        }
        out
    }

    /// A stream's content.
    pub fn read_stream(&self, e: &DirEntry) -> Result<Vec<u8>> {
        if e.size < self.cutoff {
            Ok(self.mini_chain_bytes(e.start, e.size))
        } else {
            self.chain_bytes(e.start, Some(e.size))
        }
    }
}

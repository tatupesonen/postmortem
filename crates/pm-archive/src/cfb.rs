//! Compound File Binary (OLE2, MS-CFB) reader over [`ReadAt`]: header,
//! DIFAT/FAT, mini FAT, directory and streams read lazily through their
//! sector chains (no stream is loaded whole). Chains are checked for loops
//! and out-of-range sectors.

use std::io;

use pm_core::{Bytes, ReadAt};

use crate::io::read_upto;

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// Sector numbers at or above this are markers.
const MAX_REG: u32 = 0xFFFF_FFFA;
const NOSTREAM: u32 = 0xFFFF_FFFF;
/// Most directory entries read.
const MAX_ENTRIES: usize = 1 << 18;
/// Largest FAT accepted (entries): 64 Mi sectors.
const MAX_FAT: usize = 1 << 26;

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirEntry {
    pub(crate) name: String,
    /// 1 storage, 2 stream, 5 root.
    pub(crate) kind: u8,
    left: u32,
    right: u32,
    child: u32,
    start: u32,
    pub(crate) size: u64,
}

/// An opened compound file.
#[derive(Debug)]
pub(crate) struct Cfb<'a> {
    src: &'a dyn ReadAt,
    sector: u64,
    mini: u64,
    cutoff: u64,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    dir: Vec<DirEntry>,
    /// Physical offsets of the root's mini stream sectors.
    mini_stream: Vec<u64>,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

impl<'a> Cfb<'a> {
    pub(crate) fn open(src: &'a dyn ReadAt) -> io::Result<Self> {
        let h = read_upto(src, 0, 512)?;
        if h.get(..8) != Some(&SIGNATURE[..]) || h.len() < 512 {
            return Err(bad("not a compound file"));
        }
        let shift = h.u16_le(30).map_err(|_| bad("header"))?;
        let sector: u64 = match shift {
            9 => 512,
            12 => 4096,
            _ => return Err(bad("unsupported sector size")),
        };
        let mini_shift = h.u16_le(32).unwrap_or(6);
        let mini = if mini_shift == 6 {
            64
        } else {
            return Err(bad("mini sector size"));
        };
        let cutoff = u64::from(h.u32_le(56).unwrap_or(4096));
        let first_dir = h.u32_le(48).unwrap_or(NOSTREAM);
        let first_minifat = h.u32_le(60).unwrap_or(NOSTREAM);
        let first_difat = h.u32_le(68).unwrap_or(NOSTREAM);
        let num_difat = h.u32_le(72).unwrap_or(0);
        let total_sectors = (src.size() / sector).saturating_add(1);

        // DIFAT: 109 entries in the header, then a chain of DIFAT sectors.
        let mut fat_sectors: Vec<u32> = (0..109)
            .filter_map(|i| h.u32_le(76 + i * 4).ok())
            .filter(|&s| s < MAX_REG)
            .collect();
        let mut next = first_difat;
        let per = (sector / 4) as usize;
        let mut seen = 0u32;
        while next < MAX_REG && seen <= num_difat.min(1 << 16) {
            let s = read_upto(src, (u64::from(next) + 1) * sector, sector as usize)?;
            for i in 0..per - 1 {
                if let Ok(v) = s.u32_le(i * 4)
                    && v < MAX_REG
                {
                    fat_sectors.push(v);
                }
            }
            next = s.u32_le((per - 1) * 4).unwrap_or(NOSTREAM);
            seen += 1;
        }
        let mut fat = Vec::new();
        for &fs in &fat_sectors {
            if u64::from(fs) >= total_sectors || fat.len() >= MAX_FAT {
                continue;
            }
            let s = read_upto(src, (u64::from(fs) + 1) * sector, sector as usize)?;
            fat.extend(s.chunks_exact(4).map(|c| c.u32_le(0).unwrap_or(0)));
        }
        let mut cfb = Self {
            src,
            sector,
            mini,
            cutoff,
            fat,
            minifat: Vec::new(),
            dir: Vec::new(),
            mini_stream: Vec::new(),
        };
        // Directory.
        let dir_chain = cfb.chain(first_dir)?;
        'dir: for off in dir_chain {
            let s = read_upto(src, off, sector as usize)?;
            for e in s.chunks_exact(128) {
                if cfb.dir.len() >= MAX_ENTRIES {
                    break 'dir;
                }
                let name_len = usize::from(e.u16_le(64).unwrap_or(0)).min(64);
                let units: Vec<u16> = e
                    .get(..name_len.saturating_sub(2))
                    .unwrap_or_default()
                    .chunks_exact(2)
                    .map(|c| c.u16_le(0).unwrap_or(0))
                    .collect();
                let size = if sector == 512 {
                    u64::from(e.u32_le(120).unwrap_or(0))
                } else {
                    e.u64_le(120).unwrap_or(0)
                };
                cfb.dir.push(DirEntry {
                    name: String::from_utf16_lossy(&units),
                    kind: e.get(66).copied().unwrap_or(0),
                    left: e.u32_le(68).unwrap_or(NOSTREAM),
                    right: e.u32_le(72).unwrap_or(NOSTREAM),
                    child: e.u32_le(76).unwrap_or(NOSTREAM),
                    start: e.u32_le(116).unwrap_or(NOSTREAM),
                    size,
                });
            }
        }
        // Mini FAT and mini stream.
        for off in cfb.chain(first_minifat)? {
            if cfb.minifat.len() >= MAX_FAT {
                break;
            }
            let s = read_upto(src, off, sector as usize)?;
            cfb.minifat
                .extend(s.chunks_exact(4).map(|c| c.u32_le(0).unwrap_or(0)));
        }
        if let Some(root) = cfb.dir.first().filter(|e| e.kind == 5) {
            cfb.mini_stream = cfb.chain(root.start)?;
        }
        Ok(cfb)
    }

    /// Physical offsets of a regular sector chain.
    fn chain(&self, start: u32) -> io::Result<Vec<u64>> {
        let mut out = Vec::new();
        let mut cur = start;
        let mut seen = vec![false; self.fat.len()];
        while cur < MAX_REG {
            let i = cur as usize;
            match seen.get_mut(i) {
                Some(s) if !*s => *s = true,
                Some(_) => return Err(bad("sector chain loop")),
                None => return Err(bad("sector out of range")),
            }
            out.push((u64::from(cur) + 1) * self.sector);
            cur = self.fat.get(i).copied().unwrap_or(NOSTREAM);
        }
        Ok(out)
    }

    /// Mini-stream offsets of a mini sector chain.
    fn mini_chain(&self, start: u32) -> io::Result<Vec<u64>> {
        let mut out = Vec::new();
        let mut cur = start;
        let mut seen = vec![false; self.minifat.len()];
        while cur < MAX_REG {
            let i = cur as usize;
            match seen.get_mut(i) {
                Some(s) if !*s => *s = true,
                Some(_) => return Err(bad("mini sector chain loop")),
                None => return Err(bad("mini sector out of range")),
            }
            let logical = u64::from(cur) * self.mini;
            let sec = (logical / self.sector) as usize;
            let phys = self
                .mini_stream
                .get(sec)
                .ok_or_else(|| bad("mini stream too short"))?;
            out.push(phys + logical % self.sector);
            cur = self.minifat.get(i).copied().unwrap_or(NOSTREAM);
        }
        Ok(out)
    }

    /// The streams directly under the root storage, in tree order.
    pub(crate) fn root_streams(&self) -> Vec<&DirEntry> {
        let mut out = Vec::new();
        let Some(root) = self.dir.first() else {
            return out;
        };
        let mut stack = vec![root.child];
        let mut seen = vec![false; self.dir.len()];
        while let Some(i) = stack.pop() {
            let Some(e) = self.dir.get(i as usize) else {
                continue;
            };
            if std::mem::replace(seen.get_mut(i as usize).unwrap_or(&mut true), true) {
                continue;
            }
            if e.kind == 2 {
                out.push(e);
            }
            stack.push(e.left);
            stack.push(e.right);
        }
        out
    }

    /// A root-level stream by name (case-insensitive).
    pub(crate) fn stream(&self, name: &str) -> Option<&DirEntry> {
        self.root_streams()
            .into_iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }

    /// Opens a stream for reading.
    pub(crate) fn open_stream(&self, e: &DirEntry) -> io::Result<CfbStream<'a>> {
        let (parts, unit) = if e.size < self.cutoff {
            (self.mini_chain(e.start)?, self.mini)
        } else {
            (self.chain(e.start)?, self.sector)
        };
        let size = e.size.min(parts.len() as u64 * unit);
        Ok(CfbStream {
            src: self.src,
            parts,
            unit,
            size,
        })
    }
}

/// A stream of a compound file.
#[derive(Debug)]
pub(crate) struct CfbStream<'a> {
    src: &'a dyn ReadAt,
    parts: Vec<u64>,
    unit: u64,
    size: u64,
}

impl ReadAt for CfbStream<'_> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset.saturating_add(done as u64);
            if pos >= self.size {
                break;
            }
            let idx = (pos / self.unit) as usize;
            let within = pos % self.unit;
            let Some(&phys) = self.parts.get(idx) else {
                break;
            };
            let max = (self.unit - within).min(self.size - pos);
            let want = usize::try_from(max).map_or(buf.len() - done, |m| m.min(buf.len() - done));
            let n = self.src.read_at(
                phys + within,
                buf.get_mut(done..done + want).unwrap_or_default(),
            )?;
            if n == 0 {
                break;
            }
            done += n;
        }
        Ok(done)
    }

    fn size(&self) -> u64 {
        self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    #[test]
    fn rejects_garbage() {
        assert!(Cfb::open(&MemSource::new(vec![0u8; 600])).is_err());
        let mut h = SIGNATURE.to_vec();
        h.resize(512, 0xFF);
        assert!(Cfb::open(&MemSource::new(h)).is_err());
    }
}

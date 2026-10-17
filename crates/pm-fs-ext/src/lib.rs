//! Forensic ext2/ext3/ext4 parser.
//!
//! * Block mapping through extent trees (any depth, uninitialised extents
//!   read as zeros), classic direct/indirect blocks, and ext4 inline data.
//! * Deleted names recovered from directory-entry slack (the gap a removed
//!   entry leaves inside its predecessor's `rec_len`), as Autopsy does.
//! * Allocation status from the inode and block bitmaps (respecting
//!   `INODE_UNINIT` / `BLOCK_UNINIT` groups).
//! * Nanosecond timestamps, `crtime`, and the 2038 epoch extension bits.
//!
//! Inode numbers are ext inode numbers; the root directory is inode 2.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;

use jiff::Timestamp;
use pm_core::io::MemSource;
use pm_core::{Bytes, ReadAt, SharedSource};
use pm_fs::walk::{WalkOptions, walk};
use pm_fs::{
    DirEntry, Extent, ExtentKind, ExtentSource, FileKind, FileSystem, FsError, FsInfo, FsKind,
    InodeId, MetaFlags, Metadata, Result, Times,
};

pub const ROOT_INODE: InodeId = 2;

const SUPERBLOCK_OFFSET: u64 = 1024;
const MAGIC: u16 = 0xEF53;
const EXTENT_MAGIC: u16 = 0xF30A;
const FL_EXTENTS: u32 = 0x0008_0000;
const FL_INLINE: u32 = 0x1000_0000;
const FL_INDEX: u32 = 0x0000_1000;
const INCOMPAT_FILETYPE: u32 = 0x0002;
const INCOMPAT_EXTENTS: u32 = 0x0040;
const INCOMPAT_64BIT: u32 = 0x0080;
const INCOMPAT_FLEX_BG: u32 = 0x0200;
const COMPAT_HAS_JOURNAL: u32 = 0x0004;
const BG_INODE_UNINIT: u16 = 0x1;
const BG_BLOCK_UNINIT: u16 = 0x2;
/// Refuse to map more blocks than this for one file (corruption guard).
const MAX_FILE_BLOCKS: u64 = 1 << 32;
const MAX_DIR_BYTES: u64 = 256 * 1024 * 1024;

fn corrupt(reason: impl Into<String>) -> FsError {
    FsError::Corrupt {
        fs: FsKind::Ext4,
        reason: reason.into(),
    }
}

/// Quick probe for an ext superblock.
pub fn probe(src: &dyn ReadAt) -> bool {
    let mut sb = [0u8; 1024];
    src.read_exact_at(SUPERBLOCK_OFFSET, &mut sb).is_ok() && Superblock::parse(&sb).is_ok()
}

#[derive(Debug, Clone)]
struct Superblock {
    inodes_count: u32,
    blocks_count: u64,
    first_data_block: u64,
    block_size: u64,
    blocks_per_group: u64,
    inodes_per_group: u64,
    inode_size: u64,
    desc_size: u64,
    incompat: u32,
    compat: u32,
    label: String,
    uuid: [u8; 16],
}

impl Superblock {
    fn parse(sb: &[u8]) -> std::result::Result<Self, String> {
        let e = |t: pm_core::Truncated| t.to_string();
        if sb.u16_le(56).map_err(e)? != MAGIC {
            return Err("bad ext magic".into());
        }
        let log = sb.u32_le(24).map_err(e)?;
        if log > 6 {
            return Err(format!("invalid block size log {log}"));
        }
        let rev = sb.u32_le(76).map_err(e)?;
        let incompat = sb.u32_le(96).map_err(e)?;
        let inode_size = if rev == 0 {
            128
        } else {
            u64::from(sb.u16_le(88).map_err(e)?)
        };
        if !(128..=4096).contains(&inode_size) || !inode_size.is_power_of_two() {
            return Err(format!("invalid inode size {inode_size}"));
        }
        let blocks_hi = if incompat & INCOMPAT_64BIT != 0 {
            u64::from(sb.u32_le(336).map_err(e)?)
        } else {
            0
        };
        let desc_size = if incompat & INCOMPAT_64BIT != 0 {
            u64::from(sb.u16_le(254).map_err(e)?).max(32)
        } else {
            32
        };
        let s = Self {
            inodes_count: sb.u32_le(0).map_err(e)?,
            blocks_count: u64::from(sb.u32_le(4).map_err(e)?) | (blocks_hi << 32),
            first_data_block: u64::from(sb.u32_le(20).map_err(e)?),
            block_size: 1024 << log,
            blocks_per_group: u64::from(sb.u32_le(32).map_err(e)?),
            inodes_per_group: u64::from(sb.u32_le(40).map_err(e)?),
            inode_size,
            desc_size,
            incompat,
            compat: sb.u32_le(92).map_err(e)?,
            label: pm_core::decode::cstr_lossy(sb.sub(120, 16).map_err(e)?),
            uuid: sb.array(104).map_err(e)?,
        };
        if s.blocks_per_group == 0 || s.inodes_per_group == 0 || s.inodes_count == 0 {
            return Err("zero group geometry".into());
        }
        Ok(s)
    }

    fn kind(&self) -> FsKind {
        if self.incompat & (INCOMPAT_EXTENTS | INCOMPAT_64BIT | INCOMPAT_FLEX_BG) != 0 {
            FsKind::Ext4
        } else if self.compat & COMPAT_HAS_JOURNAL != 0 {
            FsKind::Ext3
        } else {
            FsKind::Ext2
        }
    }

    fn group_count(&self) -> u64 {
        self.blocks_count
            .saturating_sub(self.first_data_block)
            .div_ceil(self.blocks_per_group)
    }
}

#[derive(Debug, Clone, Copy)]
struct Group {
    block_bitmap: u64,
    inode_bitmap: u64,
    inode_table: u64,
    flags: u16,
}

/// A decoded inode.
#[derive(Debug, Clone)]
struct Inode {
    mode: u16,
    uid: u32,
    gid: u32,
    size: u64,
    times: Times,
    dtime: u32,
    links: u16,
    flags: u32,
    block: [u8; 60],
    generation: u32,
}

impl Inode {
    fn kind(&self) -> FileKind {
        match self.mode & 0xF000 {
            0x4000 => FileKind::Directory,
            0x8000 => FileKind::Regular,
            0xA000 => FileKind::Symlink,
            0x2000 => FileKind::CharDevice,
            0x6000 => FileKind::BlockDevice,
            0x1000 => FileKind::Fifo,
            0xC000 => FileKind::Socket,
            _ => FileKind::Unknown,
        }
    }
}

fn ext_time(secs: u32, extra: Option<u32>) -> Option<Timestamp> {
    if secs == 0 && extra.unwrap_or(0) == 0 {
        return None;
    }
    let mut s = i64::from(secs as i32);
    let mut nanos = 0u32;
    if let Some(x) = extra {
        s += i64::from(x & 3) << 32;
        nanos = x >> 2;
    }
    Timestamp::new(
        s,
        i32::try_from(nanos)
            .ok()
            .filter(|n| *n < 1_000_000_000)
            .unwrap_or(0),
    )
    .ok()
}

/// An opened ext2/3/4 filesystem.
#[derive(Debug)]
pub struct ExtFs {
    src: SharedSource,
    sb: Superblock,
    groups: Vec<Group>,
    info: FsInfo,
}

impl ExtFs {
    pub fn open(src: SharedSource) -> Result<Self> {
        let raw = src.read_vec_at(SUPERBLOCK_OFFSET, 1024)?;
        let sb = Superblock::parse(&raw).map_err(corrupt)?;
        let gdt_block = sb.first_data_block + 1;
        let n = sb.group_count();
        if n > 1 << 24 {
            return Err(corrupt("implausible group count"));
        }
        let gdt = src.read_vec_at(gdt_block * sb.block_size, (n * sb.desc_size) as usize)?;
        let wide = sb.desc_size >= 64;
        let mut groups = Vec::with_capacity(n as usize);
        for g in 0..n as usize {
            let d = gdt.get(g * sb.desc_size as usize..).unwrap_or_default();
            let hi = |off: usize| {
                if wide {
                    u64::from(d.u32_le(off).unwrap_or(0)) << 32
                } else {
                    0
                }
            };
            groups.push(Group {
                block_bitmap: u64::from(d.u32_le(0)?) | hi(32),
                inode_bitmap: u64::from(d.u32_le(4)?) | hi(36),
                inode_table: u64::from(d.u32_le(8)?) | hi(40),
                flags: d.u16_le(18)?,
            });
        }
        let uuid = sb.uuid;
        let info = FsInfo {
            kind: sb.kind(),
            block_size: u32::try_from(sb.block_size).unwrap_or(4096),
            block_count: sb.blocks_count,
            root_inode: ROOT_INODE,
            first_inode: 1,
            last_inode: u64::from(sb.inodes_count),
            label: (!sb.label.is_empty()).then(|| sb.label.clone()),
            serial: Some(format!(
                "{}-{}-{}-{}-{}",
                pm_core::decode::to_hex(uuid.get(0..4).unwrap_or_default()),
                pm_core::decode::to_hex(uuid.get(4..6).unwrap_or_default()),
                pm_core::decode::to_hex(uuid.get(6..8).unwrap_or_default()),
                pm_core::decode::to_hex(uuid.get(8..10).unwrap_or_default()),
                pm_core::decode::to_hex(uuid.get(10..16).unwrap_or_default())
            )),
        };
        Ok(Self {
            src,
            sb,
            groups,
            info,
        })
    }

    fn inode(&self, ino: InodeId) -> Result<Inode> {
        if ino == 0 || ino > u64::from(self.sb.inodes_count) {
            return Err(FsError::NotFound(ino));
        }
        let idx = ino - 1;
        let g = self
            .groups
            .get((idx / self.sb.inodes_per_group) as usize)
            .ok_or(FsError::NotFound(ino))?;
        let off = g
            .inode_table
            .checked_mul(self.sb.block_size)
            .and_then(|o| o.checked_add((idx % self.sb.inodes_per_group) * self.sb.inode_size))
            .ok_or(FsError::NotFound(ino))?;
        let b = self.src.read_vec_at(off, self.sb.inode_size as usize)?;
        let extra_isize = if self.sb.inode_size > 128 {
            u64::from(b.u16_le(128).unwrap_or(0))
        } else {
            0
        };
        let has = |field_end: u64| self.sb.inode_size > 128 && 128 + extra_isize >= field_end;
        let extra = |off: usize, end: u64| if has(end) { b.u32_le(off).ok() } else { None };
        let crtime = if has(152) {
            ext_time(b.u32_le(144)?, extra(148, 152))
        } else {
            None
        };
        Ok(Inode {
            mode: b.u16_le(0)?,
            uid: u32::from(b.u16_le(2)?) | (u32::from(b.u16_le(120)?) << 16),
            gid: u32::from(b.u16_le(24)?) | (u32::from(b.u16_le(122)?) << 16),
            size: u64::from(b.u32_le(4)?) | (u64::from(b.u32_le(108)?) << 32),
            times: Times {
                accessed: ext_time(b.u32_le(8)?, extra(140, 144)),
                changed: ext_time(b.u32_le(12)?, extra(132, 136)),
                modified: ext_time(b.u32_le(16)?, extra(136, 140)),
                created: crtime,
            },
            dtime: b.u32_le(20)?,
            links: b.u16_le(26)?,
            flags: b.u32_le(32)?,
            block: b.array(40)?,
            generation: b.u32_le(100)?,
        })
    }

    fn bit(&self, block: u64, index: u64) -> Result<bool> {
        let byte = self
            .src
            .read_vec_at(block * self.sb.block_size + index / 8, 1)?;
        Ok(byte.first().is_some_and(|b| b & (1 << (index % 8)) != 0))
    }

    fn inode_allocated(&self, ino: InodeId) -> bool {
        let idx = ino.saturating_sub(1);
        let Some(g) = self.groups.get((idx / self.sb.inodes_per_group) as usize) else {
            return false;
        };
        if g.flags & BG_INODE_UNINIT != 0 {
            return false;
        }
        self.bit(g.inode_bitmap, idx % self.sb.inodes_per_group)
            .unwrap_or(false)
    }

    fn block_ptr(&self, block: u64) -> Result<Vec<u32>> {
        let b = self
            .src
            .read_vec_at(block * self.sb.block_size, self.sb.block_size as usize)?;
        Ok(b.as_chunks::<4>()
            .0
            .iter()
            .map(|&c| u32::from_le_bytes(c))
            .collect())
    }

    /// Physical extents of an inode's data (logical/physical in bytes).
    fn data_extents(&self, ino: &Inode) -> Result<Vec<Extent>> {
        let bs = self.sb.block_size;
        let limit_blocks = ino.size.div_ceil(bs).min(MAX_FILE_BLOCKS);
        let mut out: Vec<Extent> = Vec::new();
        let mut push = |logical_block: u64, phys_block: Option<u64>, count: u64| {
            if count == 0 || logical_block >= limit_blocks {
                return;
            }
            let count = count.min(limit_blocks - logical_block);
            let (kind, phys) = match phys_block {
                Some(p) => (ExtentKind::Allocated, p.saturating_mul(bs)),
                None => (ExtentKind::Sparse, 0),
            };
            if let Some(last) = out.last_mut()
                && last.kind == kind
                && kind == ExtentKind::Allocated
                && last.logical_offset + last.len == logical_block * bs
                && last.physical_offset + last.len == phys
            {
                last.len += count * bs;
                return;
            }
            out.push(Extent {
                logical_offset: logical_block * bs,
                physical_offset: phys,
                len: count * bs,
                kind,
            });
        };
        if ino.flags & FL_EXTENTS != 0 {
            let mut visited = HashSet::new();
            self.walk_extents(&ino.block, 0, &mut visited, &mut push)?;
        } else {
            let ptrs: Vec<u32> = ino
                .block
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&c| u32::from_le_bytes(c))
                .collect();
            let per = bs / 4;
            let mut lb = 0u64;
            for &p in ptrs.iter().take(12) {
                push(lb, (p != 0).then_some(u64::from(p)), 1);
                lb += 1;
            }
            // Single, double and triple indirection.
            for (level, &root) in ptrs.iter().skip(12).take(3).enumerate() {
                let span = per.pow(level as u32 + 1);
                if lb >= limit_blocks {
                    break;
                }
                self.indirect(u64::from(root), level as u32, lb, &mut push)?;
                lb += span;
            }
        }
        Ok(out)
    }

    fn indirect(
        &self,
        block: u64,
        level: u32,
        first: u64,
        push: &mut impl FnMut(u64, Option<u64>, u64),
    ) -> Result<()> {
        let per = self.sb.block_size / 4;
        let span = per.pow(level);
        if block == 0 || block >= self.sb.blocks_count {
            push(first, None, span * per);
            return Ok(());
        }
        for (i, p) in self.block_ptr(block)?.into_iter().enumerate() {
            let lb = first + i as u64 * span;
            if level == 0 {
                push(lb, (p != 0).then_some(u64::from(p)), 1);
            } else {
                self.indirect(u64::from(p), level - 1, lb, push)?;
            }
        }
        Ok(())
    }

    fn walk_extents(
        &self,
        node: &[u8],
        depth: u32,
        visited: &mut HashSet<u64>,
        push: &mut impl FnMut(u64, Option<u64>, u64),
    ) -> Result<()> {
        if node.u16_le(0)? != EXTENT_MAGIC || depth > 8 {
            return Err(corrupt("bad extent header"));
        }
        let entries = usize::from(node.u16_le(2)?);
        let tree_depth = node.u16_le(6)?;
        for i in 0..entries {
            let e = node.sub(12 + i * 12, 12)?;
            let lblock = u64::from(e.u32_le(0)?);
            if tree_depth == 0 {
                let raw_len = u64::from(e.u16_le(4)?);
                let start = (u64::from(e.u16_le(6)?) << 32) | u64::from(e.u32_le(8)?);
                // Lengths above 32768 mark uninitialised (zero-reading) extents.
                let (len, init) = if raw_len > 32_768 {
                    (raw_len - 32_768, false)
                } else {
                    (raw_len, true)
                };
                push(lblock, init.then_some(start), len);
            } else {
                let leaf = (u64::from(e.u16_le(8)?) << 32) | u64::from(e.u32_le(4)?);
                if leaf >= self.sb.blocks_count || !visited.insert(leaf) {
                    continue;
                }
                let child = self
                    .src
                    .read_vec_at(leaf * self.sb.block_size, self.sb.block_size as usize)?;
                self.walk_extents(&child, depth + 1, visited, push)?;
            }
        }
        Ok(())
    }

    /// Content source for an inode (inline data, fast symlinks, or blocks).
    fn content(&self, ino: &Inode) -> Result<SharedSource> {
        let inline = ino.flags & FL_INLINE != 0;
        let fast_link = ino.kind() == FileKind::Symlink
            && ino.size < 60
            && ino.flags & FL_EXTENTS == 0
            && ino.block.get(4..).is_some_and(|_| true)
            && self.is_fast_symlink(ino);
        if inline || fast_link {
            let n = (ino.size as usize).min(60);
            return Ok(Arc::new(MemSource::new(
                ino.block.get(..n).unwrap_or_default().to_vec(),
            )));
        }
        let extents = self.data_extents(ino)?;
        Ok(Arc::new(ExtentSource::new(
            Arc::clone(&self.src),
            extents,
            ino.size,
        )))
    }

    /// Fast symlinks keep the target in `i_block` and use no data blocks.
    fn is_fast_symlink(&self, ino: &Inode) -> bool {
        let first = u32::from_le_bytes(
            ino.block
                .get(..4)
                .and_then(|b| b.try_into().ok())
                .unwrap_or_default(),
        );
        // A slow symlink's first pointer is a valid data block; a fast one
        // holds text. Text bytes form an implausibly large block number.
        ino.size < 60 && (u64::from(first) >= self.sb.blocks_count || first == 0 && ino.size == 0)
    }

    fn file_type_kind(t: u8) -> FileKind {
        match t {
            1 => FileKind::Regular,
            2 => FileKind::Directory,
            3 => FileKind::CharDevice,
            4 => FileKind::BlockDevice,
            5 => FileKind::Fifo,
            6 => FileKind::Socket,
            7 => FileKind::Symlink,
            _ => FileKind::Unknown,
        }
    }

    /// Parses the entries of one directory block, including deleted
    /// entries hidden in record slack.
    fn parse_dir_block(
        &self,
        block: &[u8],
        dir_indexed: bool,
        first_block: bool,
        out: &mut Vec<DirEntry>,
    ) {
        let filetype = self.sb.incompat & INCOMPAT_FILETYPE != 0;
        let inodes = u64::from(self.sb.inodes_count);
        let parse = |off: usize| -> Option<(u64, usize, String, u8)> {
            let ino = u64::from(block.u32_le(off).ok()?);
            let rec_len = usize::from(block.u16_le(off + 4).ok()?);
            let (name_len, ftype) = if filetype {
                (
                    usize::from(block.u8_at(off + 6).ok()?),
                    block.u8_at(off + 7).ok()?,
                )
            } else {
                (usize::from(block.u16_le(off + 6).ok()?), 0)
            };
            let name = block.get(off + 8..off + 8 + name_len)?;
            (name_len <= 255).then(|| {
                (
                    ino,
                    rec_len,
                    String::from_utf8_lossy(name).into_owned(),
                    ftype,
                )
            })
        };
        let mut off = 0usize;
        while off + 8 <= block.len() {
            let Some((ino, rec_len, name, ftype)) = parse(off) else {
                break;
            };
            if rec_len < 8 || rec_len % 4 != 0 || off + rec_len > block.len() {
                break;
            }
            let real = (8 + name.len()).next_multiple_of(4);
            // htree internal nodes: one empty entry spanning the block.
            let dx_node = dir_indexed && ino == 0 && rec_len == block.len();
            if ino != 0 && !name.is_empty() && name != "." && name != ".." && ino <= inodes {
                out.push(DirEntry {
                    name: name.clone(),
                    short_name: None,
                    inode: ino,
                    seq: None,
                    kind: Self::file_type_kind(ftype),
                    allocated: true,
                });
            }
            // Slack after the live record may hold deleted entries.
            let skip_slack = dx_node || (dir_indexed && first_block && name == "..");
            if !skip_slack && rec_len > real + 8 {
                let mut s = off + real;
                while s + 8 <= off + rec_len {
                    match parse(s) {
                        Some((dino, drec, dname, dtype))
                            if dino != 0
                                && dino <= inodes
                                && !dname.is_empty()
                                && dname != "."
                                && dname != ".."
                                && drec >= 8
                                && drec % 4 == 0
                                && s + (8 + dname.len()).next_multiple_of(4) <= off + rec_len
                                && dname
                                    .chars()
                                    .all(|c| !c.is_control() && c != '/' && c != '\u{fffd}') =>
                        {
                            out.push(DirEntry {
                                name: dname.clone(),
                                short_name: None,
                                inode: dino,
                                seq: None,
                                kind: Self::file_type_kind(dtype),
                                allocated: false,
                            });
                            s += (8 + dname.len()).next_multiple_of(4);
                        }
                        _ => s += 4,
                    }
                }
            }
            off += rec_len;
        }
    }
}

impl FileSystem for ExtFs {
    fn info(&self) -> &FsInfo {
        &self.info
    }

    fn metadata(&self, inode: InodeId) -> Result<Metadata> {
        let ino = self.inode(inode)?;
        let mut m = Metadata::new(inode, ino.kind(), ino.size);
        m.flags = MetaFlags::empty();
        if self.inode_allocated(inode) {
            m.flags |= MetaFlags::ALLOCATED;
        }
        if ino.mode != 0 || ino.dtime != 0 {
            m.flags |= MetaFlags::USED;
        }
        m.times = ino.times;
        m.mode = Some(u32::from(ino.mode));
        m.uid = Some(ino.uid);
        m.gid = Some(ino.gid);
        m.link_count = u32::from(ino.links);
        m.seq = Some(ino.generation);
        Ok(m)
    }

    fn read_dir(&self, inode: InodeId) -> Result<Vec<DirEntry>> {
        let ino = self.inode(inode)?;
        if ino.kind() != FileKind::Directory {
            return Err(FsError::NotADirectory(inode));
        }
        let src = self.content(&ino)?;
        let size = src.size().min(MAX_DIR_BYTES);
        let data = src.read_vec_at(0, size as usize)?;
        let mut out = Vec::new();
        let indexed = ino.flags & FL_INDEX != 0;
        if ino.flags & FL_INLINE != 0 {
            // Inline directory: 4-byte parent inode, then entries.
            self.parse_dir_block(data.get(4..).unwrap_or_default(), false, false, &mut out);
        } else {
            for (i, block) in data.chunks(self.sb.block_size as usize).enumerate() {
                self.parse_dir_block(block, indexed, i == 0, &mut out);
            }
        }
        // A deleted slack entry may duplicate a live name (renames); keep live.
        let mut seen = HashSet::new();
        out.retain(|e| {
            seen.insert((e.inode, e.name.clone(), e.allocated))
                && (e.allocated || !seen.contains(&(e.inode, e.name.clone(), true)))
        });
        Ok(out)
    }

    fn open(&self, inode: InodeId, stream: Option<u32>) -> Result<SharedSource> {
        if let Some(s) = stream.filter(|&s| s != 0) {
            return Err(FsError::NoSuchStream { inode, stream: s });
        }
        self.content(&self.inode(inode)?)
    }

    fn extents(&self, inode: InodeId, _stream: Option<u32>) -> Result<Vec<Extent>> {
        let ino = self.inode(inode)?;
        if ino.flags & FL_INLINE != 0
            || (ino.kind() == FileKind::Symlink && self.is_fast_symlink(&ino))
        {
            return Ok(vec![Extent {
                logical_offset: 0,
                physical_offset: 0,
                len: ino.size.min(60),
                kind: ExtentKind::Resident,
            }]);
        }
        self.data_extents(&ino)
    }

    fn read_link(&self, inode: InodeId) -> Result<Option<String>> {
        let ino = self.inode(inode)?;
        if ino.kind() != FileKind::Symlink {
            return Ok(None);
        }
        let src = self.content(&ino)?;
        let bytes = src.read_vec_at(0, src.size().min(4096) as usize)?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    fn unallocated_blocks(&self) -> Result<Vec<(u64, u64)>> {
        let mut out: Vec<(u64, u64)> = Vec::new();
        let bpg = self.sb.blocks_per_group;
        for (gi, g) in self.groups.iter().enumerate() {
            let first = self.sb.first_data_block + gi as u64 * bpg;
            let count = bpg.min(self.sb.blocks_count.saturating_sub(first));
            let bits = if g.flags & BG_BLOCK_UNINIT != 0 {
                Vec::new()
            } else {
                self.src.read_vec_at(
                    g.block_bitmap * self.sb.block_size,
                    count.div_ceil(8) as usize,
                )?
            };
            for i in 0..count {
                let used = bits
                    .get((i / 8) as usize)
                    .is_some_and(|b| b & (1 << (i % 8)) != 0);
                if used {
                    continue;
                }
                let b = first + i;
                match out.last_mut() {
                    Some((s, n)) if *s + *n == b => *n += 1,
                    _ => out.push((b, 1)),
                }
            }
        }
        Ok(out)
    }

    fn orphans(&self) -> Result<Vec<DirEntry>> {
        // Inodes that were used (mode set) but are no longer allocated and
        // not referenced by any name: Autopsy's ext $OrphanFiles.
        let mut named = HashSet::new();
        walk(self, &WalkOptions::default(), |i| {
            named.insert(i.entry.inode);
            ControlFlow::Continue(())
        });
        let mut out = Vec::new();
        let first_regular = 11u64;
        for ino in first_regular..=u64::from(self.sb.inodes_count) {
            if named.contains(&ino) || self.inode_allocated(ino) {
                continue;
            }
            let Ok(i) = self.inode(ino) else { continue };
            if i.mode == 0 || i.dtime == 0 {
                continue;
            }
            out.push(DirEntry {
                name: format!("OrphanFile-{ino}"),
                short_name: None,
                inode: ino,
                seq: None,
                kind: i.kind(),
                allocated: false,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_time_fields() {
        // 0x7FFFFFFF + epoch bit 1 → year 2242 range; nanos in upper bits.
        let t = ext_time(10, Some(5 << 2)).unwrap();
        assert_eq!(t.as_second(), 10);
        assert_eq!(t.subsec_nanosecond(), 5);
        let t = ext_time(0x8000_0000, Some(1)).unwrap();
        assert_eq!(t.as_second(), i64::from(i32::MIN) + (1i64 << 32));
        assert_eq!(ext_time(0, None), None);
    }

    #[test]
    fn rejects_non_ext() {
        assert!(Superblock::parse(&[0u8; 1024]).is_err());
    }
}

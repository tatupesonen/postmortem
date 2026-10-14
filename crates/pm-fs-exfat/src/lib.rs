//! Forensic exFAT parser.
//!
//! * Directory entry *sets* (File + Stream Extension + File Name entries)
//!   are validated with their set checksum, which also makes deleted sets
//!   and orphan scans in unallocated space reliable.
//! * Deleted entries (in-use bit cleared) are listed with their full names.
//! * Timestamps honour the per-entry UTC offset field; when it is absent the
//!   configured time zone is used (TSK 4.15 misapplies the offset).
//! * Content follows the FAT chain, or the contiguous run for `NoFatChain`
//!   streams; bytes past the valid data length read as zeros.
//!
//! Inode numbers are the byte offset of an entry's primary directory entry
//! divided by 32 (as in `pm-fs-fat`); the root directory is [`ROOT_INODE`].

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::Arc;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use parking_lot::Mutex;
use pm_core::decode::utf16le_lossy;
use pm_core::io::{MemSource, SliceSource};
use pm_core::time::{dos_datetime, local_to_timestamp};
use pm_core::{Bytes, ReadAt, SharedSource};
use pm_fs::walk::{WalkOptions, walk};
use pm_fs::{
    DirEntry, Extent, ExtentKind, ExtentSource, FileKind, FileSystem, FsError, FsInfo, FsKind,
    InodeId, MetaFlags, Metadata, Result, Times,
};

pub const ROOT_INODE: InodeId = 2;
pub const MBR_INODE: InodeId = 3;
pub const FAT_INODE_BASE: InodeId = 4;

const ENTRY: usize = 32;
const MAX_DIR_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FAT_BYTES: u64 = 512 * 1024 * 1024;
const ORPHAN_SCAN_CHUNK: u64 = 4 * 1024 * 1024;

const T_ALLOC_BITMAP: u8 = 0x81;
const T_UPCASE: u8 = 0x82;
const T_LABEL: u8 = 0x83;
const T_FILE: u8 = 0x85;
const T_STREAM: u8 = 0xC0;
const T_NAME: u8 = 0xC1;
const IN_USE: u8 = 0x80;

fn corrupt(reason: impl Into<String>) -> FsError {
    FsError::Corrupt {
        fs: FsKind::ExFat,
        reason: reason.into(),
    }
}

#[derive(Debug, Clone)]
pub struct ExFatOptions {
    /// Zone for timestamps without a valid UTC offset.
    pub timezone: TimeZone,
}

impl Default for ExFatOptions {
    fn default() -> Self {
        Self {
            timezone: TimeZone::UTC,
        }
    }
}

#[derive(Debug, Clone)]
struct Geometry {
    bytes_per_sector: u64,
    cluster_size: u64,
    volume_sectors: u64,
    fat_offset: u64,
    fat_length: u64,
    num_fats: u32,
    active_fat: u32,
    heap_offset: u64,
    cluster_count: u32,
    root_cluster: u32,
    serial: u32,
}

impl Geometry {
    fn cluster_offset(&self, c: u32) -> u64 {
        (self.heap_offset + u64::from(c - 2) * (self.cluster_size / self.bytes_per_sector))
            * self.bytes_per_sector
    }

    fn valid(&self, c: u32) -> bool {
        c >= 2 && u64::from(c) < u64::from(self.cluster_count) + 2
    }
}

fn parse_boot(b: &[u8]) -> std::result::Result<Geometry, String> {
    let e = |t: pm_core::Truncated| t.to_string();
    if b.sub(3, 8).map_err(e)? != b"EXFAT   " {
        return Err("missing EXFAT signature".into());
    }
    if b.u16_le(510).map_err(e)? != 0xAA55 {
        return Err("missing boot signature".into());
    }
    let bps_shift = u32::from(b.u8_at(108).map_err(e)?);
    let spc_shift = u32::from(b.u8_at(109).map_err(e)?);
    if !(9..=12).contains(&bps_shift) || bps_shift + spc_shift > 25 {
        return Err(format!(
            "invalid sector/cluster shifts {bps_shift}/{spc_shift}"
        ));
    }
    let bps = 1u64 << bps_shift;
    let num_fats = u32::from(b.u8_at(110).map_err(e)?);
    if !(1..=2).contains(&num_fats) {
        return Err(format!("invalid FAT count {num_fats}"));
    }
    let flags = b.u16_le(106).map_err(e)?;
    let g = Geometry {
        bytes_per_sector: bps,
        cluster_size: bps << spc_shift,
        volume_sectors: b.u64_le(72).map_err(e)?,
        fat_offset: u64::from(b.u32_le(80).map_err(e)?),
        fat_length: u64::from(b.u32_le(84).map_err(e)?),
        num_fats,
        active_fat: u32::from(flags & 1).min(num_fats - 1),
        heap_offset: u64::from(b.u32_le(88).map_err(e)?),
        cluster_count: b.u32_le(92).map_err(e)?,
        root_cluster: b.u32_le(96).map_err(e)?,
        serial: b.u32_le(100).map_err(e)?,
    };
    if g.fat_offset == 0 || g.heap_offset <= g.fat_offset || !g.valid(g.root_cluster) {
        return Err("inconsistent exFAT geometry".into());
    }
    Ok(g)
}

/// Quick probe for an exFAT boot sector.
pub fn probe(src: &dyn ReadAt) -> bool {
    let mut b = [0u8; 512];
    src.read_exact_at(0, &mut b).is_ok() && parse_boot(&b).is_ok()
}

/// A decoded file directory entry set.
#[derive(Debug, Clone)]
struct EntrySet {
    deleted: bool,
    attributes: u16,
    created: (u32, u8, u8),
    modified: (u32, u8, u8),
    accessed: (u32, u8),
    no_fat_chain: bool,
    valid_len: u64,
    first_cluster: u32,
    data_len: u64,
    name: String,
}

impl EntrySet {
    fn is_dir(&self) -> bool {
        self.attributes & 0x10 != 0
    }
}

/// Entry set checksum (spec §6.3.3), skipping the checksum field itself.
fn set_checksum(set: &[u8]) -> u16 {
    set.iter().enumerate().fold(0u16, |sum, (i, &b)| {
        if i == 2 || i == 3 {
            sum
        } else {
            sum.rotate_right(1).wrapping_add(u16::from(b))
        }
    })
}

/// Parses the entry set starting at `buf[0]` (a File entry, live or
/// deleted). Returns the set and its length in entries.
fn parse_set(buf: &[u8]) -> Option<(EntrySet, usize)> {
    let ty = buf.u8_at(0).ok()?;
    let deleted = ty & IN_USE == 0;
    if ty & !IN_USE != T_FILE & !IN_USE {
        return None;
    }
    let secondary = usize::from(buf.u8_at(1).ok()?);
    if !(2..=18).contains(&secondary) {
        return None;
    }
    let len = (secondary + 1) * ENTRY;
    let raw = buf.get(..len)?;
    // Deleted sets had their in-use bits cleared after the checksum was
    // computed; restore them to verify.
    let mut check = raw.to_vec();
    if deleted {
        for chunk in check.chunks_exact_mut(ENTRY) {
            if let Some(t) = chunk.first_mut() {
                *t |= IN_USE;
            }
        }
    }
    if set_checksum(&check) != raw.u16_le(2).ok()? {
        return None;
    }
    let stream = raw.get(ENTRY..2 * ENTRY)?;
    if stream.u8_at(0).ok()? | IN_USE != T_STREAM {
        return None;
    }
    let name_len = usize::from(stream.u8_at(3).ok()?);
    let mut units = Vec::with_capacity(name_len);
    for k in 2..=secondary {
        let ent = raw.get(k * ENTRY..(k + 1) * ENTRY)?;
        if ent.u8_at(0).ok()? | IN_USE != T_NAME {
            continue;
        }
        for c in 0..15 {
            if units.len() < name_len {
                units.push(ent.u16_le(2 + c * 2).ok()?);
            }
        }
    }
    let name: String = char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    let flags = stream.u8_at(1).ok()?;
    Some((
        EntrySet {
            deleted,
            attributes: raw.u16_le(4).ok()?,
            created: (
                raw.u32_le(8).ok()?,
                raw.u8_at(20).ok()?,
                raw.u8_at(22).ok()?,
            ),
            modified: (
                raw.u32_le(12).ok()?,
                raw.u8_at(21).ok()?,
                raw.u8_at(23).ok()?,
            ),
            accessed: (raw.u32_le(16).ok()?, raw.u8_at(24).ok()?),
            no_fat_chain: flags & 0x02 != 0,
            valid_len: stream.u64_le(8).ok()?,
            first_cluster: stream.u32_le(20).ok()?,
            data_len: stream.u64_le(24).ok()?,
            name,
        },
        secondary + 1,
    ))
}

/// What a directory listing slot refers to.
#[derive(Debug, Clone)]
enum Node {
    Set(EntrySet),
    Bitmap { first_cluster: u32, len: u64 },
    Upcase { first_cluster: u32, len: u64 },
    Label(String),
}

/// An opened exFAT filesystem.
#[derive(Debug)]
pub struct ExFatFs {
    src: SharedSource,
    geo: Geometry,
    fat: Vec<u32>,
    /// Allocation bitmap: bit per cluster (cluster 2 = bit 0).
    bitmap: Vec<u8>,
    info: FsInfo,
    tz: TimeZone,
    /// Nodes discovered while listing, keyed by inode.
    nodes: Mutex<HashMap<InodeId, Node>>,
}

impl ExFatFs {
    pub fn open(src: SharedSource, opts: &ExFatOptions) -> Result<Self> {
        let mut b = [0u8; 512];
        src.read_exact_at(0, &mut b)?;
        let geo = parse_boot(&b).map_err(corrupt)?;
        let fat_bytes = (u64::from(geo.cluster_count) + 2) * 4;
        if fat_bytes > MAX_FAT_BYTES || fat_bytes > geo.fat_length * geo.bytes_per_sector {
            return Err(corrupt("FAT does not fit its region"));
        }
        let fat_off =
            (geo.fat_offset + u64::from(geo.active_fat) * geo.fat_length) * geo.bytes_per_sector;
        let raw = src.read_vec_at(fat_off, fat_bytes as usize)?;
        let fat: Vec<u32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&c| u32::from_le_bytes(c))
            .collect();
        let info = FsInfo {
            kind: FsKind::ExFat,
            block_size: u32::try_from(geo.bytes_per_sector).unwrap_or(512),
            block_count: geo.volume_sectors,
            root_inode: ROOT_INODE,
            first_inode: ROOT_INODE,
            last_inode: geo.volume_sectors.saturating_mul(geo.bytes_per_sector) / ENTRY as u64,
            label: None,
            serial: Some(format!(
                "{:04X}-{:04X}",
                geo.serial >> 16,
                geo.serial & 0xFFFF
            )),
        };
        let mut fs = Self {
            src,
            geo,
            fat,
            bitmap: Vec::new(),
            info,
            tz: opts.timezone.clone(),
            nodes: Mutex::new(HashMap::new()),
        };
        // The root listing names the allocation bitmap and the volume label.
        let root = fs.root_entries()?;
        for (_, node) in &root {
            match node {
                Node::Bitmap { first_cluster, len } => {
                    let clusters =
                        fs.chain(*first_cluster, len.div_ceil(fs.geo.cluster_size), false);
                    let src = fs.extent_source(&clusters, *len, *len);
                    fs.bitmap = src.read_vec_at(0, (*len).min(fat_bytes) as usize)?;
                }
                Node::Label(l) if !l.is_empty() => fs.info.label = Some(l.clone()),
                _ => {}
            }
        }
        Ok(fs)
    }

    fn allocated(&self, c: u32) -> bool {
        let bit = u64::from(c.saturating_sub(2));
        self.bitmap
            .get((bit / 8) as usize)
            .is_some_and(|b| b & (1 << (bit % 8)) != 0)
    }

    /// Cluster list of a stream: contiguous for `NoFatChain` (and deleted
    /// files, whose FAT entries may be cleared), else the FAT chain.
    fn chain(&self, first: u32, clusters: u64, contiguous: bool) -> Vec<u32> {
        let max = clusters.min(MAX_DIR_BYTES / self.geo.cluster_size.max(1)) as usize;
        let mut out = Vec::new();
        if !self.geo.valid(first) {
            return out;
        }
        if contiguous {
            for i in 0..max as u32 {
                let c = first.saturating_add(i);
                if !self.geo.valid(c) {
                    break;
                }
                out.push(c);
            }
            return out;
        }
        let mut seen = HashSet::new();
        let mut c = first;
        while out.len() < max && self.geo.valid(c) && seen.insert(c) {
            out.push(c);
            match self.fat.get(c as usize).copied() {
                Some(n) if self.geo.valid(n) => c = n,
                _ => break,
            }
        }
        out
    }

    fn cluster_extents(&self, clusters: &[u32]) -> Vec<Extent> {
        let cs = self.geo.cluster_size;
        let mut out: Vec<Extent> = Vec::new();
        for (i, &c) in clusters.iter().enumerate() {
            let phys = self.geo.cluster_offset(c);
            let logical = i as u64 * cs;
            match out.last_mut() {
                Some(e) if e.physical_offset + e.len == phys => e.len += cs,
                _ => out.push(Extent {
                    logical_offset: logical,
                    physical_offset: phys,
                    len: cs,
                    kind: ExtentKind::Allocated,
                }),
            }
        }
        out
    }

    fn extent_source(&self, clusters: &[u32], valid: u64, size: u64) -> ExtentSource {
        let mut ext = self.cluster_extents(clusters);
        // Bytes past the valid data length read as zeros.
        ext.retain_mut(|e| {
            if e.logical_offset >= valid {
                return false;
            }
            e.len = e.len.min(valid - e.logical_offset);
            true
        });
        ExtentSource::new(Arc::clone(&self.src), ext, size)
    }

    fn set_clusters(&self, s: &EntrySet) -> Vec<u32> {
        let n = s.data_len.div_ceil(self.geo.cluster_size);
        self.chain(s.first_cluster, n, s.no_fat_chain || s.deleted)
    }

    /// Raw `(offset, bytes)` of a directory's entry area.
    fn dir_raw(&self, clusters: &[u32]) -> Result<Vec<(u64, Vec<u8>)>> {
        let mut out = Vec::new();
        for ext in self.cluster_extents(clusters) {
            out.push((
                ext.physical_offset,
                self.src
                    .read_vec_at(ext.physical_offset, ext.len as usize)?,
            ));
        }
        Ok(out)
    }

    /// Parses a directory's entries into `(inode, node)` pairs.
    fn parse_dir(raw: &[(u64, Vec<u8>)]) -> Vec<(InodeId, Node)> {
        // Flatten so entry sets spanning extents are contiguous in memory.
        let mut bytes = Vec::new();
        let mut offsets = Vec::new();
        for (off, data) in raw {
            for (i, chunk) in data.chunks_exact(ENTRY).enumerate() {
                offsets.push(off + (i * ENTRY) as u64);
                bytes.extend_from_slice(chunk);
            }
        }
        let mut out = Vec::new();
        let mut i = 0usize;
        let mut ended = false;
        while i < offsets.len() {
            let at = i * ENTRY;
            let Some(&ty) = bytes.get(at) else { break };
            let inode = offsets.get(i).copied().unwrap_or(0) / ENTRY as u64;
            match ty {
                0x00 => {
                    ended = true;
                    i += 1;
                }
                T_ALLOC_BITMAP if !ended => {
                    let e = bytes.get(at..at + ENTRY).unwrap_or_default();
                    if let (Ok(fc), Ok(len)) = (e.u32_le(20), e.u64_le(24)) {
                        out.push((
                            inode,
                            Node::Bitmap {
                                first_cluster: fc,
                                len,
                            },
                        ));
                    }
                    i += 1;
                }
                T_UPCASE if !ended => {
                    let e = bytes.get(at..at + ENTRY).unwrap_or_default();
                    if let (Ok(fc), Ok(len)) = (e.u32_le(20), e.u64_le(24)) {
                        out.push((
                            inode,
                            Node::Upcase {
                                first_cluster: fc,
                                len,
                            },
                        ));
                    }
                    i += 1;
                }
                T_LABEL if !ended => {
                    let e = bytes.get(at..at + ENTRY).unwrap_or_default();
                    let n = usize::from(e.u8_at(1).unwrap_or(0)).min(11);
                    out.push((
                        inode,
                        Node::Label(utf16le_lossy(e.get(2..2 + n * 2).unwrap_or_default())),
                    ));
                    i += 1;
                }
                t if t & !IN_USE == T_FILE & !IN_USE => {
                    match parse_set(bytes.get(at..).unwrap_or_default()) {
                        // The set checksum makes acceptance safe anywhere,
                        // including after the end-of-directory marker.
                        Some((set, len)) => {
                            out.push((inode, Node::Set(set)));
                            i += len;
                        }
                        _ => i += 1,
                    }
                }
                _ => i += 1,
            }
        }
        out
    }

    fn root_entries(&self) -> Result<Vec<(InodeId, Node)>> {
        let clusters = self.chain(
            self.geo.root_cluster,
            MAX_DIR_BYTES / self.geo.cluster_size,
            false,
        );
        Ok(Self::parse_dir(&self.dir_raw(&clusters)?))
    }

    fn node(&self, inode: InodeId) -> Result<Node> {
        if let Some(n) = self.nodes.lock().get(&inode) {
            return Ok(n.clone());
        }
        // Not listed yet: try to decode an entry set in place.
        let buf = self
            .src
            .read_vec_at(inode * ENTRY as u64, 19 * ENTRY)
            .or_else(|_| self.src.read_vec_at(inode * ENTRY as u64, 3 * ENTRY))?;
        let (set, _) = parse_set(&buf).ok_or(FsError::NotFound(inode))?;
        Ok(Node::Set(set))
    }

    fn ts(&self, packed: u32, centis: u8, utc_off: u8) -> Option<Timestamp> {
        let dt = dos_datetime((packed >> 16) as u16, packed as u16, centis)?;
        if utc_off & 0x80 != 0 {
            // Signed 7-bit count of 15-minute increments east of UTC.
            let quarters = i64::from(((utc_off << 1) as i8) >> 1);
            let local = dt.to_zoned(TimeZone::UTC).ok()?.timestamp();
            local
                .checked_sub(jiff::SignedDuration::from_mins(quarters * 15))
                .ok()
        } else {
            local_to_timestamp(dt, &self.tz)
        }
    }

    fn virtual_files(&self) -> Vec<(InodeId, String, u64, u64)> {
        let bps = self.geo.bytes_per_sector;
        let mut v = vec![(MBR_INODE, "$MBR".to_owned(), 0, bps)];
        for n in 0..self.geo.num_fats {
            let off = (self.geo.fat_offset + u64::from(n) * self.geo.fat_length) * bps;
            v.push((
                FAT_INODE_BASE + u64::from(n),
                format!("$FAT{}", n + 1),
                off,
                self.geo.fat_length * bps,
            ));
        }
        v
    }

    fn remember(&self, entries: &[(InodeId, Node)]) {
        let mut map = self.nodes.lock();
        for (i, n) in entries {
            map.insert(*i, n.clone());
        }
    }

    fn to_dir_entry(inode: InodeId, node: &Node) -> DirEntry {
        let (name, kind, allocated) = match node {
            Node::Set(s) => (
                s.name.clone(),
                if s.is_dir() {
                    FileKind::Directory
                } else {
                    FileKind::Regular
                },
                !s.deleted,
            ),
            Node::Bitmap { .. } => ("$ALLOC_BITMAP".to_owned(), FileKind::Virtual, true),
            Node::Upcase { .. } => ("$UPCASE_TABLE".to_owned(), FileKind::Virtual, true),
            Node::Label(l) => (format!("{l} (Volume Label Entry)"), FileKind::Virtual, true),
        };
        DirEntry {
            name,
            short_name: None,
            inode,
            seq: None,
            kind,
            allocated,
        }
    }
}

impl FileSystem for ExFatFs {
    fn info(&self) -> &FsInfo {
        &self.info
    }

    fn metadata(&self, inode: InodeId) -> Result<Metadata> {
        if inode == ROOT_INODE {
            let n = self
                .chain(
                    self.geo.root_cluster,
                    MAX_DIR_BYTES / self.geo.cluster_size,
                    false,
                )
                .len() as u64;
            return Ok(Metadata::new(
                ROOT_INODE,
                FileKind::Directory,
                n * self.geo.cluster_size,
            ));
        }
        if let Some((ino, _, _, len)) = self.virtual_files().into_iter().find(|v| v.0 == inode) {
            return Ok(Metadata::new(ino, FileKind::Virtual, len));
        }
        Ok(match self.node(inode)? {
            Node::Set(s) => {
                let kind = if s.is_dir() {
                    FileKind::Directory
                } else {
                    FileKind::Regular
                };
                let mut m = Metadata::new(inode, kind, s.data_len);
                m.flags = MetaFlags::USED;
                if s.deleted {
                    m.flags |= MetaFlags::RECOVERED_BEST_EFFORT;
                } else {
                    m.flags |= MetaFlags::ALLOCATED;
                }
                m.attributes = Some(u32::from(s.attributes));
                m.times = Times {
                    modified: self.ts(s.modified.0, s.modified.1, s.modified.2),
                    accessed: self.ts(s.accessed.0, 0, s.accessed.1),
                    changed: None,
                    created: self.ts(s.created.0, s.created.1, s.created.2),
                };
                m
            }
            Node::Bitmap { len, .. } | Node::Upcase { len, .. } => {
                Metadata::new(inode, FileKind::Virtual, len)
            }
            Node::Label(_) => Metadata::new(inode, FileKind::Virtual, 0),
        })
    }

    fn read_dir(&self, inode: InodeId) -> Result<Vec<DirEntry>> {
        let entries = if inode == ROOT_INODE {
            self.root_entries()?
        } else {
            let Node::Set(s) = self.node(inode)? else {
                return Err(FsError::NotADirectory(inode));
            };
            if !s.is_dir() {
                return Err(FsError::NotADirectory(inode));
            }
            // A deleted directory whose first cluster was reused is gone.
            if s.deleted && self.allocated(s.first_cluster) {
                return Ok(Vec::new());
            }
            Self::parse_dir(&self.dir_raw(&self.set_clusters(&s))?)
        };
        self.remember(&entries);
        let mut out: Vec<DirEntry> = entries
            .iter()
            .map(|(i, n)| Self::to_dir_entry(*i, n))
            .collect();
        if inode == ROOT_INODE {
            out.extend(
                self.virtual_files()
                    .into_iter()
                    .map(|(ino, name, _, _)| DirEntry {
                        name,
                        short_name: None,
                        inode: ino,
                        seq: None,
                        kind: FileKind::Virtual,
                        allocated: true,
                    }),
            );
        }
        Ok(out)
    }

    fn open(&self, inode: InodeId, stream: Option<u32>) -> Result<SharedSource> {
        if let Some(s) = stream.filter(|&s| s != 0) {
            return Err(FsError::NoSuchStream { inode, stream: s });
        }
        if let Some((_, _, off, len)) = self.virtual_files().into_iter().find(|v| v.0 == inode) {
            return Ok(Arc::new(SliceSource::new(Arc::clone(&self.src), off, len)));
        }
        let extents = self.extents(inode, None)?;
        let (size, valid) = match self.node(inode).ok() {
            Some(Node::Set(s)) => (s.data_len, s.valid_len.min(s.data_len)),
            Some(Node::Bitmap { len, .. } | Node::Upcase { len, .. }) => (len, len),
            _ => (self.metadata(inode)?.size, u64::MAX),
        };
        if extents.is_empty() {
            return Ok(Arc::new(MemSource::new(Vec::new())));
        }
        let mut ext = extents;
        ext.retain_mut(|e| {
            if e.logical_offset >= valid {
                return false;
            }
            e.len = e.len.min(valid - e.logical_offset);
            true
        });
        Ok(Arc::new(ExtentSource::new(
            Arc::clone(&self.src),
            ext,
            size,
        )))
    }

    fn extents(&self, inode: InodeId, _stream: Option<u32>) -> Result<Vec<Extent>> {
        if inode == ROOT_INODE {
            let c = self.chain(
                self.geo.root_cluster,
                MAX_DIR_BYTES / self.geo.cluster_size,
                false,
            );
            return Ok(self.cluster_extents(&c));
        }
        if let Some((_, _, off, len)) = self.virtual_files().into_iter().find(|v| v.0 == inode) {
            return Ok(vec![Extent {
                logical_offset: 0,
                physical_offset: off,
                len,
                kind: ExtentKind::Allocated,
            }]);
        }
        Ok(match self.node(inode)? {
            Node::Set(s) => {
                if s.deleted && self.allocated(s.first_cluster) {
                    Vec::new() // reallocated: content is gone
                } else {
                    self.cluster_extents(&self.set_clusters(&s))
                }
            }
            Node::Bitmap { first_cluster, len } | Node::Upcase { first_cluster, len } => self
                .cluster_extents(&self.chain(
                    first_cluster,
                    len.div_ceil(self.geo.cluster_size),
                    false,
                )),
            Node::Label(_) => Vec::new(),
        })
    }

    fn unallocated_blocks(&self) -> Result<Vec<(u64, u64)>> {
        let spc = self.geo.cluster_size / self.geo.bytes_per_sector;
        let mut out: Vec<(u64, u64)> = Vec::new();
        for c in 2..u64::from(self.geo.cluster_count) + 2 {
            if self.allocated(c as u32) {
                continue;
            }
            let sector = self.geo.heap_offset + (c - 2) * spc;
            match out.last_mut() {
                Some((s, n)) if *s + *n == sector => *n += spc,
                _ => out.push((sector, spc)),
            }
        }
        Ok(out)
    }

    fn orphans(&self) -> Result<Vec<DirEntry>> {
        let mut reachable = HashSet::new();
        walk(self, &WalkOptions::default(), |i| {
            reachable.insert(i.entry.inode);
            ControlFlow::Continue(())
        });
        let mut out = Vec::new();
        let cs = self.geo.cluster_size;
        let per = (ORPHAN_SCAN_CHUNK / cs).max(1) as u32;
        let mut c = 2u32;
        let end = self.geo.cluster_count.saturating_add(2);
        while c < end {
            if self.allocated(c) {
                c += 1;
                continue;
            }
            let mut n = 1u32;
            while n < per && c + n < end && !self.allocated(c + n) {
                n += 1;
            }
            let off = self.geo.cluster_offset(c);
            let data = self.src.read_vec_at(off, (u64::from(n) * cs) as usize)?;
            // Only checksum-valid entry sets are accepted, so garbage cannot
            // masquerade as files.
            for (inode, node) in Self::parse_dir(&[(off, data)]) {
                if matches!(node, Node::Set(_)) && !reachable.contains(&inode) {
                    self.nodes.lock().insert(inode, node.clone());
                    let mut e = Self::to_dir_entry(inode, &node);
                    e.allocated = false;
                    out.push(e);
                }
            }
            c += n;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_skips_its_own_field() {
        let mut set = vec![0u8; 64];
        set[0] = T_FILE;
        let a = set_checksum(&set);
        set[2] = 0xAB;
        set[3] = 0xCD;
        assert_eq!(set_checksum(&set), a);
        set[5] = 1;
        assert_ne!(set_checksum(&set), a);
    }

    #[test]
    fn rejects_non_exfat() {
        assert!(parse_boot(&[0u8; 512]).is_err());
    }
}

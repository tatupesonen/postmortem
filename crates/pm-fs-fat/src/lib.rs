//! Forensic FAT12/16/32 parser.
//!
//! Beyond normal reading this recovers deleted entries (with their long
//! names when the LFN checksum still matches), recovers deleted file
//! content using TSK's contiguous-unallocated-cluster heuristic, exposes
//! virtual system files (`$MBR`, `$FAT1`, `$FAT2`, …) and reports free
//! clusters as unallocated space.
//!
//! # Inode numbering
//!
//! FAT has no inodes. The address of a file is its directory entry's byte
//! offset in the volume divided by 32, which is unique and stable. The root
//! directory is inode [`ROOT_INODE`]; virtual files use small fixed numbers
//! below the first possible entry address.

mod boot;
mod dirent;
mod table;

use std::sync::Arc;

use jiff::tz::TimeZone;
use pm_core::io::SliceSource;
use pm_core::time::{dos_datetime, local_to_timestamp};
use pm_core::{ReadAt, SharedSource};
use pm_fs::{
    DirEntry, Extent, ExtentKind, ExtentSource, FileKind, FileSystem, FsError, FsInfo, FsKind,
    InodeId, MetaFlags, Metadata, Result, Times,
};

use boot::Geometry;
use dirent::{ENTRY_SIZE, LfnBuilder, LfnEntry, ShortEntry};
use table::FatTable;

/// Inode of the root directory.
pub const ROOT_INODE: InodeId = 2;
/// Virtual file holding the boot sector.
pub const MBR_INODE: InodeId = 3;
/// Virtual files for each FAT copy: `FAT_INODE_BASE + n` is `$FAT{n+1}`.
pub const FAT_INODE_BASE: InodeId = 4;

/// Refuse to load FATs bigger than this into memory.
const MAX_FAT_BYTES: u64 = 512 * 1024 * 1024;
/// Upper bound on directory size (entries) to protect against loops/garbage.
const MAX_DIR_BYTES: u64 = 64 * 1024 * 1024;

/// Parsing options.
#[derive(Debug, Clone)]
pub struct FatOptions {
    /// FAT stores local wall-clock times; this zone converts them to UTC.
    pub timezone: TimeZone,
}

impl Default for FatOptions {
    fn default() -> Self {
        Self {
            timezone: TimeZone::UTC,
        }
    }
}

/// An opened FAT filesystem.
#[derive(Debug)]
pub struct FatFs {
    src: SharedSource,
    geo: Geometry,
    fat: FatTable,
    info: FsInfo,
    tz: TimeZone,
}

fn corrupt(reason: impl Into<String>) -> FsError {
    FsError::Corrupt {
        fs: FsKind::Fat32,
        reason: reason.into(),
    }
}

/// Quick check whether `src` starts with a FAT boot sector.
pub fn probe(src: &dyn ReadAt) -> bool {
    let mut b = [0u8; 512];
    src.read_exact_at(0, &mut b).is_ok() && boot::parse(&b).is_ok()
}

impl FatFs {
    pub fn open(src: SharedSource, opts: &FatOptions) -> Result<Self> {
        let mut b = [0u8; 512];
        src.read_exact_at(0, &mut b)?;
        let geo = boot::parse(&b).map_err(corrupt)?;
        let (fat_off, fat_len) = geo.fat_region(geo.active_fat);
        if fat_len > MAX_FAT_BYTES {
            return Err(FsError::Unsupported(format!(
                "FAT of {fat_len} bytes is too large"
            )));
        }
        let raw = src.read_vec_at(fat_off, fat_len as usize)?;
        let fat = FatTable::decode(&raw, geo.kind, geo.cluster_count);
        let first_entry_inode =
            geo.root_dir_sector * u64::from(geo.bytes_per_sector) / ENTRY_SIZE as u64;
        let info = FsInfo {
            kind: geo.kind,
            block_size: geo.bytes_per_sector,
            block_count: geo.total_sectors,
            root_inode: ROOT_INODE,
            first_inode: ROOT_INODE,
            last_inode: geo.fs_bytes() / ENTRY_SIZE as u64,
            label: geo.label.clone(),
            serial: geo
                .volume_id
                .map(|v| format!("{:04X}-{:04X}", v >> 16, v & 0xFFFF)),
        };
        debug_assert!(first_entry_inode > FAT_INODE_BASE + 4);
        Ok(Self {
            src,
            geo,
            fat,
            info,
            tz: opts.timezone.clone(),
        })
    }

    fn is_fat32(&self) -> bool {
        self.geo.kind == FsKind::Fat32
    }

    /// Byte extents of a cluster list, coalescing adjacent clusters.
    fn cluster_extents(&self, clusters: &[u32]) -> Vec<Extent> {
        let cs = self.geo.cluster_size();
        let mut out: Vec<Extent> = Vec::new();
        for (i, &c) in clusters.iter().enumerate() {
            let phys = self.geo.cluster_offset(c);
            let logical = i as u64 * cs;
            match out.last_mut() {
                Some(e)
                    if e.physical_offset + e.len == phys && e.logical_offset + e.len == logical =>
                {
                    e.len += cs;
                }
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

    /// Clusters holding a file's content.
    ///
    /// Allocated: the FAT chain. Deleted: TSK's heuristic — starting at the
    /// first cluster, take consecutive *unallocated* clusters (skipping
    /// allocated ones) until the size is covered; nothing if the first
    /// cluster has been reallocated.
    fn content_clusters(&self, e: &ShortEntry, size: u64) -> Vec<u32> {
        if !self.geo.is_valid_cluster(e.cluster) {
            return Vec::new();
        }
        let cs = self.geo.cluster_size();
        let needed = if e.is_dir() && size == 0 {
            (MAX_DIR_BYTES / cs) as usize
        } else {
            size.div_ceil(cs).max(1) as usize
        };
        if !e.deleted {
            return self.fat.chain(e.cluster, needed);
        }
        if !self.fat.is_free(e.cluster) {
            return Vec::new();
        }
        if e.is_dir() {
            // Deleted directory: only its first cluster is reliable.
            return vec![e.cluster];
        }
        let mut out = Vec::with_capacity(needed);
        let mut c = e.cluster;
        while out.len() < needed && self.geo.is_valid_cluster(c) {
            if self.fat.is_free(c) {
                out.push(c);
            }
            c += 1;
        }
        out
    }

    /// Raw bytes of a directory's entry area plus the byte offset of each
    /// 32-byte entry.
    fn dir_bytes(&self, inode: InodeId) -> Result<Vec<(u64, [u8; 32])>> {
        let extents = if inode == ROOT_INODE && !self.is_fat32() {
            let bps = u64::from(self.geo.bytes_per_sector);
            vec![Extent {
                logical_offset: 0,
                physical_offset: self.geo.root_dir_sector * bps,
                len: self.geo.root_dir_sectors * bps,
                kind: ExtentKind::Allocated,
            }]
        } else {
            let clusters = if inode == ROOT_INODE {
                self.fat.chain(
                    self.geo.root_cluster,
                    (MAX_DIR_BYTES / self.geo.cluster_size()) as usize,
                )
            } else {
                let e = self.entry(inode)?;
                if !e.is_dir() {
                    return Err(FsError::NotADirectory(inode));
                }
                self.content_clusters(&e, 0)
            };
            self.cluster_extents(&clusters)
        };
        let mut out = Vec::new();
        for ext in extents {
            let data = self
                .src
                .read_vec_at(ext.physical_offset, ext.len as usize)?;
            for (i, chunk) in data.as_chunks::<32>().0.iter().enumerate() {
                out.push((ext.physical_offset + (i * ENTRY_SIZE) as u64, *chunk));
            }
        }
        Ok(out)
    }

    /// Reads the short entry addressed by `inode`.
    fn entry(&self, inode: InodeId) -> Result<ShortEntry> {
        if inode <= FAT_INODE_BASE + u64::from(self.geo.num_fats) || inode > self.info.last_inode {
            return Err(FsError::NotFound(inode));
        }
        let mut buf = [0u8; 32];
        self.src
            .read_exact_at(inode * ENTRY_SIZE as u64, &mut buf)?;
        let e = ShortEntry::parse(&buf, self.is_fat32()).ok_or(FsError::NotFound(inode))?;
        if e.attr & dirent::ATTR_LFN == dirent::ATTR_LFN || buf[0] == 0 {
            return Err(FsError::NotFound(inode));
        }
        Ok(e)
    }

    fn local(&self, date: u16, time: u16, centis: u8) -> Option<jiff::Timestamp> {
        dos_datetime(date, time, centis).and_then(|dt| local_to_timestamp(dt, &self.tz))
    }

    fn entry_size(&self, e: &ShortEntry) -> u64 {
        if e.is_dir() {
            self.content_clusters(e, 0).len() as u64 * self.geo.cluster_size()
        } else {
            u64::from(e.size)
        }
    }

    fn virtual_files(&self) -> Vec<(InodeId, String, u64, u64)> {
        let bps = u64::from(self.geo.bytes_per_sector);
        let mut v = vec![(MBR_INODE, "$MBR".to_owned(), 0, bps)];
        for n in 0..self.geo.num_fats {
            let (off, len) = self.geo.fat_region(n);
            v.push((
                FAT_INODE_BASE + u64::from(n),
                format!("$FAT{}", n + 1),
                off,
                len,
            ));
        }
        v
    }

    fn root_metadata(&self) -> Metadata {
        let size = if self.is_fat32() {
            self.fat.chain(self.geo.root_cluster, usize::MAX).len() as u64 * self.geo.cluster_size()
        } else {
            self.geo.root_dir_sectors * u64::from(self.geo.bytes_per_sector)
        };
        Metadata::new(ROOT_INODE, FileKind::Directory, size)
    }
}

impl FileSystem for FatFs {
    fn info(&self) -> &FsInfo {
        &self.info
    }

    fn metadata(&self, inode: InodeId) -> Result<Metadata> {
        if inode == ROOT_INODE {
            return Ok(self.root_metadata());
        }
        if let Some((ino, _, _, len)) = self.virtual_files().into_iter().find(|v| v.0 == inode) {
            return Ok(Metadata::new(ino, FileKind::Virtual, len));
        }
        let e = self.entry(inode)?;
        let kind = if e.is_volume_label() {
            FileKind::Virtual
        } else if e.is_dir() {
            FileKind::Directory
        } else {
            FileKind::Regular
        };
        let mut flags = MetaFlags::USED;
        if e.deleted {
            flags |= MetaFlags::RECOVERED_BEST_EFFORT;
        } else {
            flags |= MetaFlags::ALLOCATED;
        }
        let mut m = Metadata::new(inode, kind, self.entry_size(&e));
        m.flags = flags;
        m.attributes = Some(u32::from(e.attr));
        m.times = Times {
            modified: self.local(e.wrt_date, e.wrt_time, 0),
            accessed: self.local(e.acc_date, 0, 0),
            changed: None,
            created: self.local(e.crt_date, e.crt_time, e.crt_tenths),
        };
        // Synthesise POSIX-like mode bits as TSK does: read-only drops write.
        let perms = if e.attr & dirent::ATTR_READ_ONLY != 0 {
            0o555
        } else {
            0o777
        };
        m.mode = Some(
            perms
                | if kind == FileKind::Directory {
                    0o040_000
                } else {
                    0o100_000
                },
        );
        Ok(m)
    }

    fn read_dir(&self, inode: InodeId) -> Result<Vec<DirEntry>> {
        let raw = self.dir_bytes(inode)?;
        let mut out = Vec::new();
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
        let fat32 = self.is_fat32();
        let mut lfn = LfnBuilder::default();
        let mut past_end = false;
        let mut first = true;
        for (off, e) in raw {
            let is_first = std::mem::replace(&mut first, false);
            if e[0] == 0 {
                past_end = true;
                lfn.clear();
                continue;
            }
            if e[11] & dirent::ATTR_LFN == dirent::ATTR_LFN {
                if let Some(l) = LfnEntry::parse(&e) {
                    lfn.push(l);
                }
                continue;
            }
            let Some(s) = ShortEntry::parse(&e, fat32) else {
                lfn.clear();
                continue;
            };
            // A reused deleted directory no longer starts with ".".
            if is_first
                && inode != ROOT_INODE
                && !s.is_dot()
                && self.entry(inode).is_ok_and(|parent| parent.deleted)
            {
                return Ok(out);
            }
            if s.is_dot() {
                lfn.clear();
                continue;
            }
            if (past_end || s.deleted) && !dirent::plausible(&s, self.geo.cluster_count) {
                lfn.clear();
                continue;
            }
            let long = lfn.take(&s);
            let short = s.short_name();
            let (name, short_name) = match long {
                Some(l) => (l, Some(short)),
                None => (short, None),
            };
            let name = if s.is_volume_label() {
                format!("{name} (Volume Label Entry)")
            } else {
                name
            };
            let kind = if s.is_volume_label() {
                FileKind::Virtual
            } else if s.is_dir() {
                FileKind::Directory
            } else {
                FileKind::Regular
            };
            out.push(DirEntry {
                name,
                short_name,
                inode: off / ENTRY_SIZE as u64,
                seq: None,
                kind,
                allocated: !s.deleted,
            });
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
        let size = self.metadata(inode)?.size;
        let extents = self.extents(inode, None)?;
        Ok(Arc::new(ExtentSource::new(
            Arc::clone(&self.src),
            extents,
            size,
        )))
    }

    fn extents(&self, inode: InodeId, _stream: Option<u32>) -> Result<Vec<Extent>> {
        if inode == ROOT_INODE {
            if self.is_fat32() {
                let c = self.fat.chain(self.geo.root_cluster, usize::MAX);
                return Ok(self.cluster_extents(&c));
            }
            let bps = u64::from(self.geo.bytes_per_sector);
            return Ok(vec![Extent {
                logical_offset: 0,
                physical_offset: self.geo.root_dir_sector * bps,
                len: self.geo.root_dir_sectors * bps,
                kind: ExtentKind::Allocated,
            }]);
        }
        if let Some((_, _, off, len)) = self.virtual_files().into_iter().find(|v| v.0 == inode) {
            return Ok(vec![Extent {
                logical_offset: 0,
                physical_offset: off,
                len,
                kind: ExtentKind::Allocated,
            }]);
        }
        let e = self.entry(inode)?;
        if e.is_volume_label() {
            return Ok(Vec::new());
        }
        let size = self.entry_size(&e);
        Ok(self.cluster_extents(&self.content_clusters(&e, size)))
    }

    fn unallocated_blocks(&self) -> Result<Vec<(u64, u64)>> {
        let spc = u64::from(self.geo.sectors_per_cluster);
        let mut out: Vec<(u64, u64)> = self
            .fat
            .free_runs()
            .into_iter()
            .map(|(c, n)| {
                (
                    self.geo.first_data_sector + u64::from(c - 2) * spc,
                    u64::from(n) * spc,
                )
            })
            .collect();
        // Sectors past the last whole cluster are never allocatable.
        let tail_start = self.geo.first_data_sector + u64::from(self.geo.cluster_count) * spc;
        if tail_start < self.geo.total_sectors {
            let tail = (tail_start, self.geo.total_sectors - tail_start);
            match out.last_mut() {
                Some(last) if last.0 + last.1 == tail.0 => last.1 += tail.1,
                _ => out.push(tail),
            }
        }
        Ok(out)
    }
}

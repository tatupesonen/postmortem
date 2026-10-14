//! Forensic NTFS parser.
//!
//! Features beyond plain reading:
//! * deleted files: unallocated MFT records are attached to their parent
//!   directory when the parent's sequence number still matches (TSK's
//!   "parent map"); otherwise they are reported as orphans;
//! * deleted names recovered from `$I30` index slack and unallocated INDX
//!   blocks;
//! * attribute lists (including a fragmented `$MFT`), torn-record tolerance,
//!   sparse and LZNT1-compressed streams, alternate data streams, both
//!   `$STANDARD_INFORMATION` and `$FILE_NAME` timestamps.
//!
//! Inode numbers are MFT record numbers.

mod boot;
mod data;
mod index;
mod lznt1;
mod record;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use pm_core::cache::ShardedLru;
use pm_core::io::MemSource;
use pm_core::time::from_filetime;
use pm_core::{Bytes, ReadAt, SharedSource};
use pm_fs::{
    DirEntry, Extent, ExtentKind, FileKind, FileSystem, FsError, FsInfo, FsKind, InodeId,
    MetaFlags, Metadata, Result, StreamInfo, StreamKind, Times,
};

use boot::Boot;
use index::IndexEntry;
use record::{
    ATTR_ATTRIBUTE_LIST, ATTR_BITMAP, ATTR_DATA, ATTR_FILE_NAME, ATTR_INDEX_ALLOCATION,
    ATTR_INDEX_ROOT, ATTR_REPARSE_POINT, ATTR_STANDARD_INFORMATION, ATTR_VOLUME_NAME, Attr,
    AttrBody, FileName, NS_DOS, Record,
};

/// Root directory MFT record.
pub const ROOT_INODE: InodeId = 5;
const BITMAP_INODE: u64 = 6;
const VOLUME_INODE: u64 = 3;
/// Fixup stride: NTFS protects every 512 bytes regardless of sector size.
const FIXUP_STRIDE: usize = 512;
/// Upper bound on resident/bitmap reads (corrupt sizes).
const MAX_META_READ: u64 = 256 * 1024 * 1024;
/// MFT bytes read per IO when scanning all records.
const SCAN_CHUNK: u64 = 4 * 1024 * 1024;
const I30: &str = "$I30";

fn corrupt(reason: impl Into<String>) -> FsError {
    FsError::Corrupt {
        fs: FsKind::Ntfs,
        reason: reason.into(),
    }
}

/// Quick check for an NTFS boot sector.
pub fn probe(src: &dyn ReadAt) -> bool {
    let mut b = [0u8; 512];
    src.read_exact_at(0, &mut b).is_ok() && boot::parse(&b).is_ok()
}

/// A record with its attributes resolved across attribute-list extensions.
#[derive(Debug)]
struct File {
    rec: Record,
    attrs: Vec<Attr>,
}

impl File {
    fn attr(&self, ty: u32, name: Option<&str>) -> Option<&Attr> {
        self.attrs
            .iter()
            .find(|a| a.ty == ty && a.name.as_deref() == name)
    }

    /// The preferred `$FILE_NAME`: Win32 or POSIX over DOS.
    fn best_name(&self) -> Option<FileName> {
        let names: Vec<FileName> = self
            .attrs
            .iter()
            .filter(|a| a.ty == ATTR_FILE_NAME)
            .filter_map(|a| a.resident().and_then(FileName::parse))
            .collect();
        names
            .iter()
            .find(|n| n.namespace != NS_DOS)
            .or(names.first())
            .cloned()
    }
}

/// Unallocated records grouped by the parent they name.
#[derive(Debug, Default)]
struct ParentMap {
    children: HashMap<u64, Vec<(u64, u16, FileName)>>,
}

/// An opened NTFS filesystem.
#[derive(Debug)]
pub struct NtfsFs {
    src: SharedSource,
    boot: Boot,
    info: FsInfo,
    mft: SharedSource,
    mft_records: u64,
    cache: ShardedLru<u64, Arc<File>>,
    parent_map: OnceLock<ParentMap>,
}

impl NtfsFs {
    pub fn open(src: SharedSource) -> Result<Self> {
        let mut b = [0u8; 512];
        src.read_exact_at(0, &mut b)?;
        let boot = boot::parse(&b).map_err(corrupt)?;
        let rs = u64::from(boot.mft_record_size);

        // Bootstrap: record 0 at the MFT's first cluster describes the MFT.
        let mft_off = boot
            .mft_lcn
            .checked_mul(boot.cluster_size)
            .ok_or_else(|| corrupt("MFT offset overflows"))?;
        let raw0 = src.read_vec_at(mft_off, rs as usize)?;
        let rec0 = record::parse_record(raw0, 0, FIXUP_STRIDE)
            .map_err(corrupt)?
            .ok_or_else(|| corrupt("MFT record 0 is not a FILE record"))?;
        let provisional = Self::stream_of(
            &src,
            &boot,
            rec0.attrs
                .iter()
                .find(|a| a.ty == ATTR_DATA && a.name.is_none()),
        )
        .ok_or_else(|| corrupt("$MFT has no $DATA"))?;
        // Resolve a possible attribute list on $MFT using the first fragment.
        let file0 = Self::resolve(&src, &boot, &provisional, rec0)?;
        let mft_attr = file0
            .attr(ATTR_DATA, None)
            .ok_or_else(|| corrupt("$MFT has no $DATA"))?;
        let mft = Self::stream_of(&src, &boot, Some(mft_attr))
            .ok_or_else(|| corrupt("$MFT $DATA unreadable"))?;
        // A corrupt $MFT size cannot describe more records than fit on the volume.
        let mft_records = mft.size().min(src.size()) / rs;

        let mut fs = Self {
            src,
            info: FsInfo {
                kind: FsKind::Ntfs,
                block_size: u32::try_from(boot.cluster_size).unwrap_or(u32::MAX),
                block_count: boot.cluster_count(),
                root_inode: ROOT_INODE,
                first_inode: 0,
                last_inode: mft_records.saturating_sub(1),
                label: None,
                serial: Some(format!("{:016X}", boot.serial)),
            },
            boot,
            mft,
            mft_records,
            cache: ShardedLru::new(16, 64),
            parent_map: OnceLock::new(),
        };
        fs.info.label = fs
            .file(VOLUME_INODE)
            .ok()
            .and_then(|f| {
                f.attr(ATTR_VOLUME_NAME, None)
                    .and_then(Attr::resident)
                    .map(pm_core::decode::utf16le_lossy)
            })
            .filter(|l| !l.is_empty());
        Ok(fs)
    }

    /// A byte source for an attribute's content.
    fn stream_of(src: &SharedSource, boot: &Boot, attr: Option<&Attr>) -> Option<SharedSource> {
        let attr = attr?;
        Some(match &attr.body {
            AttrBody::Resident(v) => Arc::new(MemSource::new(v.clone())),
            AttrBody::NonResident {
                runs,
                real_size,
                init_size,
                compression_unit,
                ..
            } => {
                if attr.flags & record::FLAG_COMPRESSED != 0 && *compression_unit > 0 {
                    Arc::new(data::CompressedStream::new(
                        Arc::clone(src),
                        runs.clone(),
                        boot.cluster_size,
                        *compression_unit,
                        *real_size,
                    ))
                } else {
                    data::plain_stream(
                        Arc::clone(src),
                        runs,
                        boot.cluster_size,
                        *real_size,
                        *init_size,
                    )
                }
            }
        })
    }

    fn read_attr(src: &SharedSource, boot: &Boot, attr: &Attr) -> Result<Vec<u8>> {
        let s = Self::stream_of(src, boot, Some(attr))
            .ok_or_else(|| corrupt("unreadable attribute"))?;
        Ok(s.read_vec_at(0, s.size().min(MAX_META_READ) as usize)?)
    }

    fn raw_record(mft: &dyn ReadAt, boot: &Boot, n: u64) -> Result<Option<Record>> {
        let rs = u64::from(boot.mft_record_size);
        let off = n.checked_mul(rs).ok_or(FsError::NotFound(n))?;
        let buf = match mft.read_vec_at(off, rs as usize) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        record::parse_record(buf, n, FIXUP_STRIDE).map_err(corrupt)
    }

    /// Merges attributes from attribute-list extension records and joins
    /// fragments of non-resident attributes.
    fn resolve(src: &SharedSource, boot: &Boot, mft: &SharedSource, rec: Record) -> Result<File> {
        let mut attrs: Vec<Attr> = rec
            .attrs
            .iter()
            .filter(|a| a.ty != ATTR_ATTRIBUTE_LIST)
            .cloned()
            .collect();
        if let Some(list) = rec.attrs.iter().find(|a| a.ty == ATTR_ATTRIBUTE_LIST) {
            let bytes = Self::read_attr(src, boot, list)?;
            let mut extensions = HashSet::new();
            let mut off = 0usize;
            while off + 26 <= bytes.len() {
                let len = usize::from(bytes.u16_le(off + 4)?);
                let mref = bytes.u64_le(off + 16)? & 0x0000_FFFF_FFFF_FFFF;
                if len < 26 {
                    break;
                }
                if mref != rec.number {
                    extensions.insert(mref);
                }
                off += len;
            }
            for ext in extensions.into_iter().take(4096) {
                if let Ok(Some(e)) = Self::raw_record(&**mft, boot, ext)
                    && e.base_ref == rec.number
                {
                    attrs.extend(e.attrs.into_iter().filter(|a| a.ty != ATTR_ATTRIBUTE_LIST));
                }
            }
        }
        Ok(File {
            attrs: merge_fragments(attrs),
            rec,
        })
    }

    fn file(&self, n: u64) -> Result<Arc<File>> {
        self.cache.get_or_try_insert(&n, || {
            let rec = Self::raw_record(&*self.mft, &self.boot, n)?.ok_or(FsError::NotFound(n))?;
            if rec.base_ref != 0 && rec.base_ref != n {
                return Err(FsError::NotFound(n)); // extension record, not a file
            }
            Ok(Arc::new(Self::resolve(
                &self.src, &self.boot, &self.mft, rec,
            )?))
        })
    }

    fn data_attr(f: &File, stream: Option<u32>) -> Option<Attr> {
        f.attrs
            .iter()
            .find(|a| {
                (a.ty == ATTR_DATA || (stream.is_some() && is_named_index(a)))
                    && match stream {
                        None => a.name.is_none(),
                        Some(id) => u32::from(a.id) == id,
                    }
            })
            .cloned()
    }

    /// Scans the whole MFT once for unallocated records (deleted files).
    fn parent_map(&self) -> &ParentMap {
        self.parent_map.get_or_init(|| {
            let mut map = ParentMap::default();
            let rs = u64::from(self.boot.mft_record_size);
            let per = (SCAN_CHUNK / rs).max(1);
            let mut n = 0u64;
            while n < self.mft_records {
                let count = per.min(self.mft_records - n);
                let Ok(block) = self.mft.read_vec_at(n * rs, (count * rs) as usize) else {
                    n += count;
                    continue;
                };
                for (i, raw) in block.chunks_exact(rs as usize).enumerate() {
                    let num = n + i as u64;
                    let Ok(Some(rec)) = record::parse_record(raw.to_vec(), num, FIXUP_STRIDE)
                    else {
                        continue;
                    };
                    if rec.in_use || rec.base_ref != 0 {
                        continue;
                    }
                    let names: Vec<FileName> = rec
                        .attrs
                        .iter()
                        .filter(|a| a.ty == ATTR_FILE_NAME)
                        .filter_map(|a| a.resident().and_then(FileName::parse))
                        .filter(|f| f.namespace != NS_DOS)
                        .collect();
                    for fname in names {
                        map.children
                            .entry(fname.parent)
                            .or_default()
                            .push((num, rec.seq, fname));
                    }
                }
                n += count;
            }
            map
        })
    }

    /// Directory entries from `$I30` (root, allocation blocks and slack).
    fn index_entries(&self, f: &File) -> Result<Vec<IndexEntry>> {
        let mut out = Vec::new();
        let root = f
            .attr(ATTR_INDEX_ROOT, Some(I30))
            .and_then(Attr::resident)
            .ok_or(FsError::NotADirectory(f.rec.number))?;
        let block_size = root.u32_le(8).map_or(self.boot.index_record_size, |s| s);
        let ctx = index::NodeCtx {
            dir: f.rec.number,
            last_inum: self.mft_records.saturating_sub(1),
            deleted: false,
        };
        index::parse_node(root, 16, ctx, &mut out);
        if let Some(alloc) = f.attr(ATTR_INDEX_ALLOCATION, Some(I30)) {
            let bytes = Self::read_attr(&self.src, &self.boot, alloc)?;
            let bitmap = f
                .attr(ATTR_BITMAP, Some(I30))
                .and_then(|a| Self::read_attr(&self.src, &self.boot, a).ok())
                .unwrap_or_default();
            let bs = (block_size as usize).max(512);
            for (k, block) in bytes.chunks_exact(bs).enumerate() {
                if block.get(..4) != Some(b"INDX".as_slice()) {
                    continue;
                }
                let mut block = block.to_vec();
                if record::apply_fixups(&mut block, FIXUP_STRIDE).is_err() {
                    continue;
                }
                let in_use = bitmap.get(k / 8).is_some_and(|b| b & (1 << (k % 8)) != 0);
                // Everything in an unused INDX block is a remnant.
                let ctx = index::NodeCtx {
                    deleted: !in_use,
                    ..ctx
                };
                index::parse_node(&block, 24, ctx, &mut out);
            }
        }
        Ok(out)
    }
}

/// A named index other than the directory index (`$Secure:$SDH`).
fn is_named_index(a: &Attr) -> bool {
    a.ty == ATTR_INDEX_ROOT && a.name.as_deref().is_some_and(|n| n != "$I30")
}

/// Joins fragments of the same non-resident attribute (split across
/// extension records) into one attribute with a continuous runlist.
fn merge_fragments(attrs: Vec<Attr>) -> Vec<Attr> {
    let mut out: Vec<Attr> = Vec::with_capacity(attrs.len());
    let mut frags: Vec<Attr> = Vec::new();
    for a in attrs {
        match &a.body {
            AttrBody::NonResident { start_vcn, .. } if *start_vcn > 0 => frags.push(a),
            _ => out.push(a),
        }
    }
    frags.sort_by_key(|a| match &a.body {
        AttrBody::NonResident { start_vcn, .. } => *start_vcn,
        AttrBody::Resident(_) => 0,
    });
    for frag in frags {
        let target = out.iter_mut().find(|a| {
            a.ty == frag.ty && a.name == frag.name && matches!(a.body, AttrBody::NonResident { .. })
        });
        if let (Some(t), AttrBody::NonResident { runs: more, .. }) = (target, frag.body)
            && let AttrBody::NonResident { runs, .. } = &mut t.body
        {
            runs.extend(more);
        }
    }
    out
}

fn times_from(b: &[u8], base: usize) -> Times {
    let t = |o: usize| b.u64_le(base + o).ok().and_then(from_filetime);
    // NTFS order: created, modified, MFT modified (changed), accessed.
    Times {
        created: t(0),
        modified: t(8),
        changed: t(16),
        accessed: t(24),
    }
}

fn fn_times(f: &FileName) -> Times {
    Times {
        created: from_filetime(f.times[0]),
        modified: from_filetime(f.times[1]),
        changed: from_filetime(f.times[2]),
        accessed: from_filetime(f.times[3]),
    }
}

impl FileSystem for NtfsFs {
    fn info(&self) -> &FsInfo {
        &self.info
    }

    fn metadata(&self, inode: InodeId) -> Result<Metadata> {
        let f = self.file(inode)?;
        let reparse_symlink = f
            .attr(ATTR_REPARSE_POINT, None)
            .and_then(Attr::resident)
            .and_then(|r| r.u32_le(0).ok())
            .is_some_and(|tag| tag == 0xA000_000C);
        let kind = if f.rec.is_dir {
            FileKind::Directory
        } else if reparse_symlink {
            FileKind::Symlink
        } else {
            FileKind::Regular
        };
        let data = f.attr(ATTR_DATA, None);
        let size = match (kind, data) {
            (_, Some(d)) => d.real_size(),
            (FileKind::Directory, None) => f
                .attr(ATTR_INDEX_ALLOCATION, Some(I30))
                .or_else(|| f.attr(ATTR_INDEX_ROOT, Some(I30)))
                .map_or(0, Attr::real_size),
            _ => 0,
        };
        let mut m = Metadata::new(inode, kind, size);
        m.flags = MetaFlags::USED;
        if f.rec.in_use {
            m.flags |= MetaFlags::ALLOCATED;
        }
        if let Some(si) = f
            .attr(ATTR_STANDARD_INFORMATION, None)
            .and_then(Attr::resident)
        {
            m.times = times_from(si, 0);
            m.attributes = si.u32_le(32).ok();
        }
        if let Some(d) = data {
            if d.flags & record::FLAG_COMPRESSED != 0 {
                m.flags |= MetaFlags::COMPRESSED;
            }
            if d.flags & record::FLAG_ENCRYPTED != 0 {
                m.flags |= MetaFlags::ENCRYPTED;
            }
            if d.flags & record::FLAG_SPARSE != 0 {
                m.flags |= MetaFlags::SPARSE;
            }
        }
        m.fn_times = f.best_name().map(|n| fn_times(&n));
        m.seq = Some(u32::from(f.rec.seq));
        m.link_count = u32::from(f.rec.link_count);
        Ok(m)
    }

    /// TSK's rule: NTFS increments an MFT entry's sequence number when it
    /// is freed, so a deleted record matches a name carrying the previous
    /// sequence; any other mismatch means the entry was reused.
    fn name_matches_meta(&self, entry: &DirEntry, meta: &Metadata) -> bool {
        let (Some(name_seq), Some(seq)) = (entry.seq, meta.seq) else {
            return true;
        };
        let expected = if meta.is_allocated() {
            seq
        } else {
            seq.saturating_sub(1)
        };
        name_seq == expected
    }

    fn read_dir(&self, inode: InodeId) -> Result<Vec<DirEntry>> {
        let dir = self.file(inode)?;
        if !dir.rec.is_dir {
            return Err(FsError::NotADirectory(inode));
        }
        let mut seen: HashSet<(u64, String)> = HashSet::new();
        let mut out = Vec::new();
        let mut entries = self.index_entries(&dir)?;
        // Live entries first so a slack duplicate never hides them.
        entries.sort_by_key(|e| e.from_slack);
        for e in entries {
            if e.name.namespace == NS_DOS || (e.mft == inode && e.name.name == ".") {
                continue;
            }
            if !seen.insert((e.mft, e.name.name.clone())) {
                continue;
            }
            let is_dir = e.name.flags & 0x1000_0000 != 0;
            out.push(DirEntry {
                name: e.name.name,
                short_name: None,
                inode: e.mft,
                seq: Some(u32::from(e.seq)),
                kind: if is_dir {
                    FileKind::Directory
                } else {
                    FileKind::Regular
                },
                allocated: !e.from_slack,
            });
        }
        // Deleted records that still name this directory as their parent.
        if let Some(children) = self.parent_map().children.get(&inode) {
            for (child, child_seq, fname) in children {
                if fname.parent_seq != dir.rec.seq || !seen.insert((*child, fname.name.clone())) {
                    continue;
                }
                let is_dir = self.file(*child).is_ok_and(|c| c.rec.is_dir);
                out.push(DirEntry {
                    name: fname.name.clone(),
                    short_name: None,
                    inode: *child,
                    // The record's sequence was bumped when it was freed;
                    // the name refers to the one before (as TSK reports).
                    seq: Some(u32::from(child_seq.saturating_sub(1))),
                    kind: if is_dir {
                        FileKind::Directory
                    } else {
                        FileKind::Regular
                    },
                    allocated: false,
                });
            }
        }
        Ok(out)
    }

    fn streams(&self, inode: InodeId) -> Result<Vec<StreamInfo>> {
        let f = self.file(inode)?;
        Ok(f.attrs
            .iter()
            .filter(|a| a.ty == ATTR_DATA || is_named_index(a))
            .map(|a| StreamInfo {
                id: u32::from(a.id),
                name: a.name.clone(),
                kind: if a.ty == ATTR_INDEX_ROOT {
                    StreamKind::Index
                } else if a.name.is_some() {
                    StreamKind::AlternateData
                } else {
                    StreamKind::Data
                },
                size: a.real_size(),
            })
            .collect())
    }

    fn open(&self, inode: InodeId, stream: Option<u32>) -> Result<SharedSource> {
        let f = self.file(inode)?;
        let attr = Self::data_attr(&f, stream);
        match (attr, stream) {
            (Some(a), _) => Self::stream_of(&self.src, &self.boot, Some(&a))
                .ok_or_else(|| corrupt("unreadable $DATA")),
            (None, None) => Ok(Arc::new(MemSource::new(Vec::new()))),
            (None, Some(s)) => Err(FsError::NoSuchStream { inode, stream: s }),
        }
    }

    fn extents(&self, inode: InodeId, stream: Option<u32>) -> Result<Vec<Extent>> {
        let f = self.file(inode)?;
        let Some(a) = Self::data_attr(&f, stream) else {
            return Ok(Vec::new());
        };
        Ok(match &a.body {
            AttrBody::Resident(v) => vec![Extent {
                logical_offset: 0,
                physical_offset: 0,
                len: v.len() as u64,
                kind: ExtentKind::Resident,
            }],
            AttrBody::NonResident {
                runs, alloc_size, ..
            } => data::runs_to_extents(runs, self.boot.cluster_size, *alloc_size),
        })
    }

    fn unallocated_blocks(&self) -> Result<Vec<(u64, u64)>> {
        let f = self.file(BITMAP_INODE)?;
        let a = f
            .attr(ATTR_DATA, None)
            .ok_or_else(|| corrupt("$Bitmap has no $DATA"))?;
        let bits = Self::read_attr(&self.src, &self.boot, a)?;
        // Never trust the boot sector's count beyond what the bitmap covers.
        let clusters = self.boot.cluster_count().min(bits.len() as u64 * 8);
        let mut out: Vec<(u64, u64)> = Vec::new();
        for c in 0..clusters {
            let used = bits
                .get((c / 8) as usize)
                .is_some_and(|b| b & (1 << (c % 8)) != 0);
            if used {
                continue;
            }
            match out.last_mut() {
                Some((s, n)) if *s + *n == c => *n += 1,
                _ => out.push((c, 1)),
            }
        }
        Ok(out)
    }

    fn orphans(&self) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        for (parent, children) in &self.parent_map().children {
            let parent_ok = |seq: u16| {
                self.file(*parent)
                    .is_ok_and(|p| p.rec.is_dir && p.rec.seq == seq)
            };
            for (child, seq, fname) in children {
                if parent_ok(fname.parent_seq) {
                    continue;
                }
                let is_dir = self.file(*child).is_ok_and(|c| c.rec.is_dir);
                out.push(DirEntry {
                    name: fname.name.clone(),
                    short_name: None,
                    inode: *child,
                    // Freed records carry the next sequence (see TSK).
                    seq: Some(u32::from(seq.saturating_sub(1))),
                    kind: if is_dir {
                        FileKind::Directory
                    } else {
                        FileKind::Regular
                    },
                    allocated: false,
                });
            }
        }
        out.sort_by_key(|e| e.inode);
        out.dedup_by_key(|e| e.inode);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_testkit::ntfs::{Deleted, FileOpts, NtfsBuilder};

    fn fs_with(b: &NtfsBuilder) -> NtfsFs {
        NtfsFs::open(Arc::new(MemSource::new(b.build()))).unwrap()
    }

    #[test]
    fn slack_scanner_finds_deleted_entry_in_root_and_indx_blocks() {
        // Small directory: slack lives in the resident $INDEX_ROOT.
        let mut b = NtfsBuilder::new(8);
        b.dir("small");
        b.file("small/live.txt", b"x");
        b.file_with(
            "small/gone.txt",
            b"y",
            FileOpts {
                deleted: Deleted::InSlack,
                ..Default::default()
            },
        );
        // Large directory: slack lives in an INDX block.
        b.dir("big");
        for i in 0..60 {
            b.file(&format!("big/entry_{i:02}_padding_padding.txt"), b"z");
        }
        b.file_with(
            "big/zz_gone.txt",
            b"w",
            FileOpts {
                deleted: Deleted::InSlack,
                ..Default::default()
            },
        );
        let fs = fs_with(&b);
        for dir_name in ["small", "big"] {
            let dir = fs
                .read_dir(ROOT_INODE)
                .unwrap()
                .into_iter()
                .find(|e| e.name == dir_name)
                .unwrap();
            let f = fs.file(dir.inode).unwrap();
            let entries = fs.index_entries(&f).unwrap();
            let slack: Vec<_> = entries
                .iter()
                .filter(|e| e.from_slack)
                .map(|e| e.name.name.as_str())
                .collect();
            let want = if dir_name == "small" {
                "gone.txt"
            } else {
                "zz_gone.txt"
            };
            assert_eq!(slack, [want], "{dir_name}");
            assert_eq!(
                f.attr(ATTR_INDEX_ALLOCATION, Some(I30)).is_some(),
                dir_name == "big"
            );
        }
    }
}

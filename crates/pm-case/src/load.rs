//! Adding disk images to a case.
//!
//! Loading is built for very large evidence (ADR-014):
//!
//! * Directories are listed in parallel on a dedicated thread pool
//!   (work-stealing, depth-first per worker, so the pending frontier stays
//!   small) while **one** writer thread inserts rows in large transactions.
//! * File ids are pre-assigned from the case database's shared allocator
//!   (`CaseDb::file_ids`, also used for derived files), so a directory's
//!   children can be produced before its row reaches the database. A
//!   directory's rows are sent *before* its subdirectories are scheduled,
//!   which keeps parents ahead of children in the writer's queue.
//! * The queue to the writer is bounded: producers wait rather than buffer
//!   unbounded numbers of rows in memory.
//! * Secondary indexes are dropped for large loads and rebuilt afterwards.
//! * Cancelling or failing removes the partially added data source, as
//!   Autopsy reverts a cancelled "Add Data Source".

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crossbeam_channel::{Sender, bounded};
use parking_lot::Mutex;
use pm_casedb::{
    CaseDb, DataSourceKind, FileIds, FileKindCode, NewDataSource, NewFile, NewFileSystem,
    NewVolume, Origin,
};
use pm_evidence::Evidence;
use pm_fs::walk::{Ancestors, MAX_LISTINGS_PER_DIR};
use pm_fs::{DirEntry, FileKind, FileSystem, FsError, FsKind, InodeId, Metadata, StreamKind};
use pm_image::HashAlgo;
use pm_volume::{SlotKind, VsKind};

use crate::case::{OpenSource, open_options};
use crate::content::layout_json;
use crate::{Case, CaseError, Result};

/// TSK's default maximum size of one unallocated-space file.
pub const DEFAULT_UNALLOC_CHUNK: u64 = 500_000_000;
const MAX_DEPTH: usize = 512;
/// Directory batches buffered between the walkers and the writer.
const QUEUE_BATCHES: usize = 256;
const VISITED_SHARDS: usize = 64;

/// Options for adding a disk image (Autopsy's image panel settings).
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Display name; defaults to the first segment's file name.
    pub name: Option<String>,
    /// IANA time zone for file systems that store local times (FAT).
    pub timezone: String,
    /// Create `$Unalloc` files covering unallocated blocks.
    pub process_unallocated: bool,
    /// Maximum size of one unallocated-space file (0 = one per run).
    pub unalloc_chunk: u64,
    /// List orphan files on FAT file systems (slow on large volumes).
    pub fat_orphans: bool,
    /// Walker threads (0 = available parallelism).
    pub threads: usize,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            name: None,
            timezone: "UTC".into(),
            process_unallocated: true,
            unalloc_chunk: DEFAULT_UNALLOC_CHUNK,
            fat_orphans: true,
            threads: 0,
        }
    }
}

/// Live progress of a load, shared with the UI.
#[derive(Debug, Default)]
pub struct LoadProgress {
    /// Rows produced so far.
    pub files: AtomicU64,
    /// Directories listed so far.
    pub dirs: AtomicU64,
    stage: Mutex<String>,
    cancel: AtomicBool,
}

impl LoadProgress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Human-readable description of the current step.
    pub fn stage(&self) -> String {
        self.stage.lock().clone()
    }

    fn set_stage(&self, s: impl Into<String>) {
        *self.stage.lock() = s.into();
    }
}

/// Result of a successful load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadSummary {
    pub ds_id: i64,
    /// Case ids of the file systems found, in evidence order.
    pub fs_ids: Vec<i64>,
    pub files: u64,
    /// Non-fatal problems (unreadable directories, …).
    pub warnings: Vec<String>,
}

/// Adds the disk image whose first segment (or only file) is `path`.
pub fn add_image(
    case: &Case,
    path: &Path,
    opts: &LoadOptions,
    progress: &LoadProgress,
) -> Result<LoadSummary> {
    progress.set_stage("Opening image");
    let image = pm_image::open(path).map_err(pm_evidence::EvidenceError::from)?;
    progress.set_stage("Reading volume system");
    let evidence = pm_evidence::analyze(image, &open_options(&opts.timezone))?;
    add_evidence(case, evidence, opts, progress)
}

/// Adds already opened evidence (e.g. a raw image given as explicit
/// segments).
pub fn add_evidence(
    case: &Case,
    evidence: Evidence,
    opts: &LoadOptions,
    progress: &LoadProgress,
) -> Result<LoadSummary> {
    let db = case.db();
    let (ds_id, fs_ids) = add_structure(db, &evidence, opts)?;
    let estimate: u64 = evidence
        .filesystems
        .iter()
        .map(|f| estimate_files(f.fs.info()))
        .sum();
    let existing = u64::try_from(db.next_file_id()? - 1).unwrap_or(0);
    let deferred = should_defer_indexes(existing, estimate);
    if deferred {
        db.defer_file_indexes()?;
    }
    let outcome = load_files(db, ds_id, &evidence, &fs_ids, opts, progress);
    let outcome = match outcome {
        Ok(w) if progress.is_cancelled() => Err((CaseError::Cancelled, w)),
        Ok(w) => Ok(w),
        Err(e) => Err((e, Vec::new())),
    };
    if let Err((e, _)) = &outcome {
        tracing::warn!(ds_id, "load failed ({e}); removing partial data source");
        progress.set_stage("Reverting");
        db.delete_data_source(ds_id)?;
    }
    if deferred {
        progress.set_stage("Indexing");
        db.rebuild_file_indexes()?;
    }
    let warnings = outcome.map_err(|(e, _)| e)?;
    let filesystems = fs_ids
        .iter()
        .zip(&evidence.filesystems)
        .map(|(id, f)| (*id, Arc::clone(&f.fs)))
        .collect();
    case.register_source(
        ds_id,
        OpenSource {
            evidence,
            filesystems,
        },
    );
    progress.set_stage("Done");
    Ok(LoadSummary {
        ds_id,
        fs_ids,
        files: progress.files.load(Ordering::Relaxed),
        warnings,
    })
}

/// Drop and rebuild indexes only when the load is large relative to what
/// the case already holds (rebuilding is proportional to the whole table).
pub fn should_defer_indexes(existing_rows: u64, estimated_new: u64) -> bool {
    estimated_new >= 100_000 && estimated_new.saturating_mul(2) >= existing_rows
}

/// Rough upper bound of the rows a file system will produce.
fn estimate_files(info: &pm_fs::FsInfo) -> u64 {
    let inodes = info.last_inode.saturating_sub(info.first_inode) + 1;
    let blocks = info.block_count.saturating_mul(u64::from(info.block_size)) / 4096;
    inodes.min(blocks.max(1))
}

/// Inserts the data source, volume system, volumes and file systems.
fn add_structure(db: &CaseDb, ev: &Evidence, opts: &LoadOptions) -> Result<(i64, Vec<i64>)> {
    let info = &ev.image.info;
    let hash = |algo: HashAlgo| {
        info.stored_hashes
            .iter()
            .find(|(a, _)| *a == algo)
            .map(|(_, h)| h.to_lowercase())
    };
    let name = opts.name.clone().unwrap_or_else(|| {
        info.paths
            .first()
            .and_then(|p| p.file_name())
            .map_or_else(|| "image".into(), |n| n.to_string_lossy().into_owned())
    });
    let ds = NewDataSource {
        name,
        kind: DataSourceKind::DiskImage,
        paths: info.paths.iter().map(|p| p.display().to_string()).collect(),
        size: info.size,
        sector_size: info.sector_size,
        timezone: opts.timezone.clone(),
        image_format: Some(info.format.to_string()),
        md5: hash(HashAlgo::Md5),
        sha1: hash(HashAlgo::Sha1),
        sha256: hash(HashAlgo::Sha256),
        acquisition: info.metadata.clone(),
        added_at: jiff::Timestamp::now().as_second(),
    };
    Ok(db.bulk(|b| {
        let ds_id = b.add_data_source(&ds)?;
        let mut volume_ids = Vec::new();
        if let Some(vs) = &ev.volume_system {
            let kind = match vs.kind {
                VsKind::Dos => 0,
                VsKind::Gpt => 1,
            };
            let guid = vs.disk_guid.map(|g| g.to_string());
            let vs_id = b.add_volume_system(ds_id, kind, vs.sector_size, guid.as_deref())?;
            for p in &vs.partitions {
                let kind = match p.kind {
                    SlotKind::Allocated => 0,
                    SlotKind::Unallocated => 1,
                    SlotKind::Meta => 2,
                };
                let id = b.add_volume(
                    vs_id,
                    &NewVolume {
                        addr: p.addr,
                        kind,
                        start_sector: p.start_sector,
                        sector_count: p.sector_count,
                        description: p.description.clone(),
                        name: p.name.clone(),
                        flags: p.flags,
                    },
                )?;
                volume_ids.push((p.addr, id));
            }
        }
        let mut fs_ids = Vec::new();
        for f in &ev.filesystems {
            let fi = f.fs.info();
            let volume_id = f.partition.as_ref().and_then(|p| {
                volume_ids
                    .iter()
                    .find(|(addr, _)| *addr == p.addr)
                    .map(|(_, id)| *id)
            });
            fs_ids.push(b.add_file_system(&NewFileSystem {
                ds_id,
                volume_id,
                kind: fi.kind.name().into(),
                byte_offset: f.offset,
                block_size: fi.block_size,
                block_count: fi.block_count,
                root_inode: fi.root_inode,
                label: fi.label.clone(),
                serial: fi.serial.clone(),
            })?);
        }
        Ok((ds_id, fs_ids))
    })?)
}

/// A directory scheduled for listing.
struct Dir {
    inode: InodeId,
    /// Case file id of the directory row.
    id: i64,
    /// Path of the directory, `/`-terminated (its children's `parent_path`).
    path: Arc<str>,
    depth: usize,
    /// Directories from the root to this one (cycle detection).
    ancestors: Arc<Ancestors>,
}

/// State shared by the walkers of one file system.
struct Walk<'a> {
    fs: &'a dyn FileSystem,
    ds_id: i64,
    fs_id: i64,
    next_id: &'a FileIds,
    tx: Sender<Vec<NewFile>>,
    progress: &'a LoadProgress,
    /// Times each directory has been listed (capped, see
    /// [`MAX_LISTINGS_PER_DIR`]).
    listings: Vec<Mutex<HashMap<InodeId, u8>>>,
    warnings: Mutex<Vec<String>>,
    /// Whether to look for alternate data streams.
    streams: bool,
    /// Set when the writer has gone away (database error).
    failed: AtomicBool,
}

impl Walk<'_> {
    fn id(&self) -> i64 {
        self.next_id.next()
    }

    fn stopped(&self) -> bool {
        self.progress.is_cancelled() || self.failed.load(Ordering::Relaxed)
    }

    /// Whether `inode` may be listed once more (TSK lists a directory under
    /// every name that reaches it; the cap stops crafted lattices).
    fn may_list(&self, inode: InodeId) -> bool {
        let shard = (inode.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) as usize % VISITED_SHARDS;
        self.listings.get(shard).is_some_and(|s| {
            let mut s = s.lock();
            let n = s.entry(inode).or_default();
            *n < MAX_LISTINGS_PER_DIR && {
                *n += 1;
                true
            }
        })
    }

    fn send(&self, rows: Vec<NewFile>) {
        if rows.is_empty() {
            return;
        }
        self.progress
            .files
            .fetch_add(rows.len() as u64, Ordering::Relaxed);
        if self.tx.send(rows).is_err() {
            self.failed.store(true, Ordering::Relaxed);
        }
    }

    fn warn(&self, what: String) {
        tracing::debug!("{what}");
        self.warnings.lock().push(what);
    }

    /// Lists `dir`, sends its rows, then schedules its subdirectories.
    fn list<'s>(&'s self, dir: &Dir, scope: &rayon::Scope<'s>) {
        if self.stopped() {
            return;
        }
        self.progress.dirs.fetch_add(1, Ordering::Relaxed);
        match self.fs.read_dir(dir.inode) {
            Ok(entries) => self.entries(dir, entries, scope),
            Err(e) => self.warn(format!("{}: {e}", dir.path)),
        }
    }

    /// Rows for the names in `parent`; descends into directories.
    fn entries<'s>(&'s self, parent: &Dir, entries: Vec<DirEntry>, scope: &rayon::Scope<'s>) {
        let mut rows = Vec::with_capacity(entries.len());
        let mut subdirs = Vec::new();
        for e in entries {
            let meta = pm_fs::entry_metadata(self.fs, &e);
            let id = self.id();
            let descend = matches!(&meta, Ok(m) if pm_fs::should_descend(&e, m));
            if descend
                && parent.depth < MAX_DEPTH
                && !parent.ancestors.contains(e.inode)
                && self.may_list(e.inode)
            {
                subdirs.push(Dir {
                    inode: e.inode,
                    id,
                    path: Arc::from(format!("{}{}/", parent.path, e.name)),
                    depth: parent.depth + 1,
                    ancestors: parent.ancestors.child(e.inode),
                });
            }
            let row = self.row(id, parent, &e, &meta);
            let mut keep_base = true;
            if self.streams && meta.is_ok() {
                keep_base = self.stream_rows(&row, &mut rows);
            }
            if keep_base {
                rows.push(row);
            }
        }
        self.send(rows);
        for d in subdirs {
            scope.spawn(move |s| self.list(&d, s));
        }
    }

    fn row(
        &self,
        id: i64,
        parent: &Dir,
        e: &DirEntry,
        meta: &Result<Metadata, FsError>,
    ) -> NewFile {
        let mut f = NewFile {
            id: Some(id),
            ds_id: self.ds_id,
            fs_id: Some(self.fs_id),
            parent_id: Some(parent.id),
            origin: Origin::FileSystem,
            inode: Some(e.inode),
            stream: None,
            name: e.name.clone(),
            parent_path: parent.path.to_string(),
            kind: kind_code(e.kind),
            name_alloc: e.allocated,
            meta_alloc: false,
            meta_flags: 0,
            size: 0,
            times: [None; 4],
            mode: None,
            uid: None,
            gid: None,
            seq: e.seq,
            attributes: None,
            derived_from: None,
            local_path: None,
            layout: None,
        };
        if let Ok(m) = meta {
            apply_meta(&mut f, m);
        }
        f
    }

    /// Named streams appear as sibling rows named `file:stream` (alternate
    /// data streams and named NTFS indexes). Returns whether the plain
    /// name gets a row too: like TSK, not for a non-directory that has only
    /// named streams (`$Secure` → `$Secure:$SDS`, `:$SDH`, `:$SII`).
    fn stream_rows(&self, file: &NewFile, out: &mut Vec<NewFile>) -> bool {
        let Some(inode) = file.inode else {
            return true;
        };
        let Ok(streams) = self.fs.streams(inode) else {
            return true;
        };
        let has_default = streams.iter().any(|s| s.kind == StreamKind::Data);
        let mut named = 0;
        for s in streams.into_iter().filter(|s| s.kind != StreamKind::Data) {
            let mut r = file.clone();
            r.id = Some(self.id());
            r.name = format!("{}:{}", file.name, s.name.unwrap_or_default());
            r.stream = Some(s.id);
            r.kind = FileKindCode::Regular;
            r.size = s.size;
            out.push(r);
            named += 1;
        }
        has_default || named == 0 || file.kind == FileKindCode::Directory
    }
}

fn kind_code(k: FileKind) -> FileKindCode {
    match k {
        FileKind::Regular => FileKindCode::Regular,
        FileKind::Directory => FileKindCode::Directory,
        FileKind::Symlink => FileKindCode::Symlink,
        FileKind::CharDevice => FileKindCode::CharDevice,
        FileKind::BlockDevice => FileKindCode::BlockDevice,
        FileKind::Fifo => FileKindCode::Fifo,
        FileKind::Socket => FileKindCode::Socket,
        FileKind::Virtual => FileKindCode::Virtual,
        FileKind::Unknown => FileKindCode::Unknown,
    }
}

/// Unix seconds and non-negative nanoseconds.
fn split_time(t: Option<jiff::Timestamp>) -> Option<(i64, u32)> {
    let ns = t?.as_nanosecond();
    let secs = i64::try_from(ns.div_euclid(1_000_000_000)).ok()?;
    let sub = u32::try_from(ns.rem_euclid(1_000_000_000)).ok()?;
    Some((secs, sub))
}

fn apply_meta(f: &mut NewFile, m: &Metadata) {
    f.kind = kind_code(m.kind);
    f.meta_alloc = m.is_allocated();
    f.meta_flags = u32::from(m.flags.bits());
    f.size = m.size;
    f.times = [
        split_time(m.times.modified),
        split_time(m.times.accessed),
        split_time(m.times.changed),
        split_time(m.times.created),
    ];
    f.mode = m.mode;
    f.uid = m.uid;
    f.gid = m.gid;
    f.seq = m.seq.or(f.seq);
    f.attributes = m.attributes;
}

/// A virtual directory row (`$OrphanFiles`, `$Unalloc`).
fn virtual_dir(id: i64, ds_id: i64, fs_id: i64, parent: &Dir, name: &str) -> NewFile {
    NewFile {
        id: Some(id),
        ds_id,
        fs_id: Some(fs_id),
        parent_id: Some(parent.id),
        origin: Origin::VirtualDir,
        inode: None,
        stream: None,
        name: name.into(),
        parent_path: parent.path.to_string(),
        kind: FileKindCode::Directory,
        name_alloc: true,
        meta_alloc: true,
        meta_flags: 0,
        size: 0,
        times: [None; 4],
        mode: None,
        uid: None,
        gid: None,
        seq: None,
        attributes: None,
        derived_from: None,
        local_path: None,
        layout: None,
    }
}

/// Walks every file system, feeding one writer; returns warnings.
fn load_files(
    db: &CaseDb,
    ds_id: i64,
    ev: &Evidence,
    fs_ids: &[i64],
    opts: &LoadOptions,
    progress: &LoadProgress,
) -> Result<Vec<String>> {
    let threads = if opts.threads == 0 {
        std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
    } else {
        opts.threads
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("pm-load-{i}"))
        .build()
        .map_err(|e| CaseError::NotFound(format!("thread pool: {e}")))?;
    let (tx, rx) = bounded::<Vec<NewFile>>(QUEUE_BATCHES);
    let mut warnings = Vec::new();

    std::thread::scope(|s| -> Result<()> {
        let writer = s.spawn(move || {
            db.bulk(|b| {
                for batch in rx {
                    for f in &batch {
                        b.add_file(f)?;
                    }
                }
                Ok(())
            })
        });
        for (found, &fs_id) in ev.filesystems.iter().zip(fs_ids) {
            if progress.is_cancelled() {
                break;
            }
            let fs = &*found.fs;
            let info = fs.info();
            let where_ = found
                .partition
                .as_ref()
                .map_or_else(String::new, |p| format!(" (volume {})", p.addr));
            progress.set_stage(format!("Adding files from {}{where_}", info.kind));
            let walk = Walk {
                fs,
                ds_id,
                fs_id,
                next_id: db.file_ids(),
                tx: tx.clone(),
                progress,
                listings: (0..VISITED_SHARDS)
                    .map(|_| Mutex::new(HashMap::new()))
                    .collect(),
                warnings: Mutex::new(Vec::new()),
                streams: info.kind == FsKind::Ntfs,
                failed: AtomicBool::new(false),
            };
            load_fs(&walk, found.offset, &pool, opts, progress);
            warnings.extend(walk.warnings.into_inner());
            if walk.failed.load(Ordering::Relaxed) {
                break;
            }
        }
        drop(tx);
        writer
            .join()
            .map_err(|_| CaseError::NotFound("writer thread panicked".into()))??;
        Ok(())
    })?;
    Ok(warnings)
}

/// Root, tree, `$OrphanFiles` and `$Unalloc` of one file system.
fn load_fs(
    walk: &Walk<'_>,
    fs_offset: u64,
    pool: &rayon::ThreadPool,
    opts: &LoadOptions,
    progress: &LoadProgress,
) {
    let fs = walk.fs;
    let info = fs.info();
    let root_inode = fs.root();
    let root = Dir {
        inode: root_inode,
        id: walk.id(),
        path: Arc::from("/"),
        depth: 0,
        ancestors: Ancestors::root(root_inode),
    };
    let root_entry = DirEntry {
        name: String::new(),
        short_name: None,
        inode: root_inode,
        seq: None,
        kind: FileKind::Directory,
        allocated: true,
    };
    let root_id = root.id;
    let mut root_row = walk.row(root_id, &root, &root_entry, &fs.metadata(root_inode));
    root_row.parent_id = None;
    walk.send(vec![root_row]);
    pool.scope(|s| walk.list(&root, s));

    let is_fat = matches!(info.kind, FsKind::Fat12 | FsKind::Fat16 | FsKind::Fat32);
    let root_dir = Dir {
        inode: root_inode,
        id: root_id,
        path: Arc::from("/"),
        depth: 0,
        ancestors: Ancestors::root(root_inode),
    };
    if !walk.stopped() && (!is_fat || opts.fat_orphans) {
        progress.set_stage(format!("Finding orphan files on {}", info.kind));
        let orphan_dir = Dir {
            inode: InodeId::MAX,
            id: walk.id(),
            path: Arc::from("/$OrphanFiles/"),
            depth: 1,
            ancestors: Ancestors::root(InodeId::MAX),
        };
        walk.send(vec![virtual_dir(
            orphan_dir.id,
            walk.ds_id,
            walk.fs_id,
            &root_dir,
            "$OrphanFiles",
        )]);
        match fs.orphans() {
            Ok(entries) => pool.scope(|s| walk.entries(&orphan_dir, entries, s)),
            Err(e) => walk.warn(format!("$OrphanFiles: {e}")),
        }
    }
    if !walk.stopped() && opts.process_unallocated {
        progress.set_stage(format!("Adding unallocated space of {}", info.kind));
        match fs.unallocated_blocks() {
            Ok(runs) if !runs.is_empty() => {
                let dir = Dir {
                    inode: InodeId::MAX,
                    id: walk.id(),
                    path: Arc::from("/$Unalloc/"),
                    depth: 1,
                    ancestors: Ancestors::root(InodeId::MAX),
                };
                let mut rows = vec![virtual_dir(
                    dir.id, walk.ds_id, walk.fs_id, &root_dir, "$Unalloc",
                )];
                let bs = u64::from(info.block_size);
                let ranges = runs.iter().map(|&(first, count)| {
                    (
                        fs_offset.saturating_add(first.saturating_mul(bs)),
                        count.saturating_mul(bs),
                    )
                });
                for chunk in chunk_ranges(ranges, opts.unalloc_chunk) {
                    rows.push(unalloc_file(walk, &dir, &chunk));
                }
                walk.send(rows);
            }
            Ok(_) => {}
            Err(e) => walk.warn(format!("unallocated blocks: {e}")),
        }
    }
}

fn unalloc_file(walk: &Walk<'_>, dir: &Dir, ranges: &[(u64, u64)]) -> NewFile {
    let start = ranges.first().map_or(0, |r| r.0);
    let end = ranges.last().map_or(0, |r| r.0 + r.1);
    let size = ranges.iter().map(|r| r.1).sum();
    let mut f = virtual_dir(walk.id(), walk.ds_id, walk.fs_id, dir, "");
    f.name = format!("Unalloc_{}_{start}_{end}", walk.fs_id);
    f.origin = Origin::UnallocBlocks;
    f.kind = FileKindCode::Regular;
    f.name_alloc = false;
    f.meta_alloc = false;
    f.size = size;
    f.layout = Some(layout_json(ranges));
    f
}

/// Groups byte ranges into files of at most `chunk` bytes, splitting runs
/// at chunk boundaries (`chunk == 0`: one file per range).
pub fn chunk_ranges(
    ranges: impl IntoIterator<Item = (u64, u64)>,
    chunk: u64,
) -> Vec<Vec<(u64, u64)>> {
    let mut out = Vec::new();
    let mut cur: Vec<(u64, u64)> = Vec::new();
    let mut cur_len = 0u64;
    for (mut off, mut len) in ranges {
        if chunk == 0 {
            if len > 0 {
                out.push(vec![(off, len)]);
            }
            continue;
        }
        while len > 0 {
            let take = len.min(chunk - cur_len);
            cur.push((off, take));
            cur_len += take;
            off += take;
            len -= take;
            if cur_len == chunk {
                out.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

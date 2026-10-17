//! In-memory NTFS volume builder for tests.
//!
//! Produces small but structurally complete NTFS 3.1 volumes: boot sector
//! (+ backup), all system files, B-tree `$I30` indexes (root + INDX blocks),
//! resident and non-resident data, fragmented and LZNT1-compressed streams,
//! alternate data streams, and deleted files in three flavours (index slack,
//! unindexed unallocated record, orphan). Validated against the reference tools.

use std::collections::BTreeMap;

use crate::lznt1;

/// Default FILETIME for all timestamps: 2021-03-04 10:20:30 UTC.
pub const DEFAULT_FILETIME: u64 = 132_593_268_300_000_000;
const RECORD: usize = 1024;
const INDEX_BLOCK: usize = 4096;
const FIRST_USER_RECORD: u64 = 64;
const ROOT: u64 = 5;

/// How a deleted file remains on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Deleted {
    /// Live file.
    #[default]
    No,
    /// Record unallocated; its old index entry survives in the parent's index slack.
    InSlack,
    /// Record unallocated; no index entry (found via its `$FILE_NAME` parent).
    Unindexed,
    /// Record unallocated and the parent's sequence number no longer matches.
    Orphan,
}

/// Options for a file.
#[derive(Debug, Clone, Default)]
pub struct FileOpts {
    /// Force resident (`Some(true)`) or non-resident (`Some(false)`) data.
    pub resident: Option<bool>,
    pub compressed: bool,
    /// Split non-resident data into two runs with a free cluster between.
    pub fragmented: bool,
    pub ads: Vec<(String, Vec<u8>)>,
    pub deleted: Deleted,
    /// created, modified, MFT-modified, accessed (FILETIME).
    pub times: Option<[u64; 4]>,
}

#[derive(Debug, Clone)]
enum Kind {
    Dir,
    File(Vec<u8>, FileOpts),
}

/// Builder; add nodes with absolute `/`-separated paths (parents first).
#[derive(Debug, Clone)]
pub struct NtfsBuilder {
    cluster_size: usize,
    total_clusters: u64,
    label: String,
    serial: u64,
    nodes: Vec<(String, Kind)>,
}

impl NtfsBuilder {
    /// A volume of `mib` MiB with 4 KiB clusters.
    pub fn new(mib: u64) -> Self {
        Self {
            cluster_size: 4096,
            total_clusters: mib * 256,
            label: "PMNTFS".into(),
            serial: 0x1234_5678_9ABC_DEF0,
            nodes: Vec::new(),
        }
    }

    pub fn label(mut self, label: &str) -> Self {
        self.label = label.into();
        self
    }

    pub fn dir(&mut self, path: &str) -> &mut Self {
        self.nodes.push((path.into(), Kind::Dir));
        self
    }

    pub fn file(&mut self, path: &str, data: &[u8]) -> &mut Self {
        self.file_with(path, data, FileOpts::default())
    }

    pub fn file_with(&mut self, path: &str, data: &[u8], opts: FileOpts) -> &mut Self {
        self.nodes
            .push((path.into(), Kind::File(data.to_vec(), opts)));
        self
    }

    pub fn build(&self) -> Vec<u8> {
        Image::new(self).finish()
    }
}

// ---------------------------------------------------------------- encoding

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn pad8(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(8) {
        v.push(0);
    }
}

fn put(buf: &mut [u8], off: usize, bytes: &[u8]) {
    buf[off..off + bytes.len()].copy_from_slice(bytes);
}

fn upcase_key(name: &str) -> Vec<u16> {
    name.encode_utf16()
        .map(|u| {
            char::from_u32(u32::from(u))
                .map(|c| {
                    let mut up = c.to_uppercase();
                    match (up.next(), up.next()) {
                        (Some(x), None) if (x as u32) < 0x10000 => x as u32 as u16,
                        _ => u,
                    }
                })
                .unwrap_or(u)
        })
        .collect()
}

/// Minimal little-endian byte count for an unsigned value (≥1).
fn ubytes(v: u64) -> usize {
    (((64 - v.leading_zeros()) as usize).div_ceil(8)).max(1)
}

/// Minimal byte count for a signed value.
fn sbytes(v: i64) -> usize {
    (1..=8)
        .find(|&n| {
            let bits = 8 * n as u32;
            let min = -(1i128 << (bits - 1));
            let max = (1i128 << (bits - 1)) - 1;
            (min..=max).contains(&i128::from(v))
        })
        .unwrap_or(8)
}

/// A data run: `len` clusters at `lcn` (`None` = sparse).
#[derive(Debug, Clone, Copy)]
struct Run {
    len: u64,
    lcn: Option<u64>,
}

fn encode_runs(runs: &[Run]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut prev: i64 = 0;
    for r in runs {
        let lb = ubytes(r.len);
        match r.lcn {
            Some(lcn) => {
                let delta = lcn as i64 - prev;
                prev = lcn as i64;
                let ob = sbytes(delta);
                out.push((ob << 4 | lb) as u8);
                out.extend_from_slice(&r.len.to_le_bytes()[..lb]);
                out.extend_from_slice(&delta.to_le_bytes()[..ob]);
            }
            None => {
                out.push(lb as u8);
                out.extend_from_slice(&r.len.to_le_bytes()[..lb]);
            }
        }
    }
    out.push(0);
    out
}

fn resident_attr(ty: u32, name: Option<&str>, value: &[u8], id: u16, indexed: bool) -> Vec<u8> {
    let name_bytes = name.map(utf16).unwrap_or_default();
    let mut a = vec![0u8; 24];
    a.extend_from_slice(&name_bytes);
    pad8(&mut a);
    let voff = a.len();
    a.extend_from_slice(value);
    pad8(&mut a);
    let len = a.len() as u32;
    put(&mut a, 0, &ty.to_le_bytes());
    put(&mut a, 4, &len.to_le_bytes());
    a[8] = 0;
    a[9] = (name_bytes.len() / 2) as u8;
    put(&mut a, 10, &24u16.to_le_bytes());
    put(&mut a, 14, &id.to_le_bytes());
    put(&mut a, 16, &(value.len() as u32).to_le_bytes());
    put(&mut a, 20, &(voff as u16).to_le_bytes());
    a[22] = u8::from(indexed);
    a
}

#[allow(clippy::too_many_arguments)]
fn nonresident_attr(
    ty: u32,
    name: Option<&str>,
    runs: &[Run],
    alloc: u64,
    real: u64,
    id: u16,
    flags: u16,
    cu: u16,
) -> Vec<u8> {
    let header = if cu > 0 { 72 } else { 64 };
    let name_bytes = name.map(utf16).unwrap_or_default();
    let mut a = vec![0u8; header];
    let name_off = a.len();
    a.extend_from_slice(&name_bytes);
    pad8(&mut a);
    let runs_off = a.len();
    a.extend_from_slice(&encode_runs(runs));
    pad8(&mut a);
    let total: u64 = runs.iter().map(|r| r.len).sum();
    let len = a.len() as u32;
    put(&mut a, 0, &ty.to_le_bytes());
    put(&mut a, 4, &len.to_le_bytes());
    a[8] = 1;
    a[9] = (name_bytes.len() / 2) as u8;
    put(&mut a, 10, &(name_off as u16).to_le_bytes());
    put(&mut a, 12, &flags.to_le_bytes());
    put(&mut a, 14, &id.to_le_bytes());
    put(&mut a, 16, &0u64.to_le_bytes());
    put(&mut a, 24, &total.saturating_sub(1).to_le_bytes());
    put(&mut a, 32, &(runs_off as u16).to_le_bytes());
    put(&mut a, 34, &cu.to_le_bytes());
    put(&mut a, 40, &alloc.to_le_bytes());
    put(&mut a, 48, &real.to_le_bytes());
    put(&mut a, 56, &real.to_le_bytes());
    if cu > 0 {
        let stored: u64 = runs.iter().filter(|r| r.lcn.is_some()).map(|r| r.len).sum();
        put(&mut a, 64, &stored.to_le_bytes());
    }
    a
}

fn std_info(times: [u64; 4], attrs: u32) -> Vec<u8> {
    let mut v = vec![0u8; 72];
    for (i, t) in times.iter().enumerate() {
        put(&mut v, i * 8, &t.to_le_bytes());
    }
    put(&mut v, 32, &attrs.to_le_bytes());
    v
}

fn file_name(
    parent: u64,
    parent_seq: u16,
    name: &str,
    times: [u64; 4],
    alloc: u64,
    real: u64,
    flags: u32,
) -> Vec<u8> {
    let n = utf16(name);
    let mut v = vec![0u8; 66];
    put(
        &mut v,
        0,
        &(parent | (u64::from(parent_seq) << 48)).to_le_bytes(),
    );
    for (i, t) in times.iter().enumerate() {
        put(&mut v, 8 + i * 8, &t.to_le_bytes());
    }
    put(&mut v, 40, &alloc.to_le_bytes());
    put(&mut v, 48, &real.to_le_bytes());
    put(&mut v, 56, &flags.to_le_bytes());
    v[64] = (n.len() / 2) as u8;
    v[65] = 1; // Win32 namespace
    v.extend_from_slice(&n);
    v
}

/// Applies update-sequence fixups (USA at `usa_off`) to a multi-sector buffer.
fn protect(buf: &mut [u8], usa_off: usize) {
    let count = buf.len() / 512 + 1;
    put(buf, 4, &(usa_off as u16).to_le_bytes());
    put(buf, 6, &(count as u16).to_le_bytes());
    let usn = 1u16;
    put(buf, usa_off, &usn.to_le_bytes());
    for i in 1..count {
        let pos = i * 512 - 2;
        let orig = [buf[pos], buf[pos + 1]];
        put(buf, usa_off + i * 2, &orig);
        put(buf, pos, &usn.to_le_bytes());
    }
}

fn mft_record(num: u64, seq: u16, links: u16, flags: u16, attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut r = vec![0u8; RECORD];
    put(&mut r, 0, b"FILE");
    put(&mut r, 16, &seq.to_le_bytes());
    put(&mut r, 18, &links.to_le_bytes());
    put(&mut r, 20, &56u16.to_le_bytes());
    put(&mut r, 22, &flags.to_le_bytes());
    let mut off = 56;
    for a in attrs {
        assert!(
            off + a.len() + 8 <= RECORD,
            "record {num} overflows ({} bytes)",
            off + a.len() + 8
        );
        put(&mut r, off, a);
        off += a.len();
    }
    put(&mut r, off, &0xFFFF_FFFFu32.to_le_bytes());
    off += 8;
    put(&mut r, 24, &(off as u32).to_le_bytes());
    put(&mut r, 28, &(RECORD as u32).to_le_bytes());
    put(&mut r, 40, &(attrs.len() as u16).to_le_bytes());
    put(&mut r, 44, &(num as u32).to_le_bytes());
    protect(&mut r, 48);
    r
}

/// One `$I30` index entry.
#[derive(Debug, Clone)]
struct IxEntry {
    key: Vec<u16>,
    mref: u64,
    fname: Vec<u8>,
}

fn entry_bytes(e: &IxEntry, subnode: Option<u64>) -> Vec<u8> {
    let mut v = vec![0u8; 16];
    v.extend_from_slice(&e.fname);
    pad8(&mut v);
    if subnode.is_some() {
        v.extend_from_slice(&[0u8; 8]);
    }
    let len = v.len();
    put(&mut v, 0, &e.mref.to_le_bytes());
    put(&mut v, 8, &(len as u16).to_le_bytes());
    put(&mut v, 10, &(e.fname.len() as u16).to_le_bytes());
    put(&mut v, 12, &u16::from(subnode.is_some()).to_le_bytes());
    if let Some(vcn) = subnode {
        put(&mut v, len - 8, &vcn.to_le_bytes());
    }
    v
}

fn end_entry(subnode: Option<u64>) -> Vec<u8> {
    let len = if subnode.is_some() { 24 } else { 16 };
    let mut v = vec![0u8; len];
    put(&mut v, 8, &(len as u16).to_le_bytes());
    put(
        &mut v,
        12,
        &(2u16 | u16::from(subnode.is_some())).to_le_bytes(),
    );
    if let Some(vcn) = subnode {
        put(&mut v, 16, &vcn.to_le_bytes());
    }
    v
}

/// Node header + entries + optional slack bytes after the end marker.
fn node(entries: &[u8], slack: &[u8], has_children: bool, alloc_total: Option<usize>) -> Vec<u8> {
    let used = 16 + entries.len();
    let alloc = alloc_total.unwrap_or(used + slack.len());
    let mut v = vec![0u8; 16];
    put(&mut v, 0, &16u32.to_le_bytes());
    put(&mut v, 4, &(used as u32).to_le_bytes());
    put(&mut v, 8, &(alloc as u32).to_le_bytes());
    v[12] = u8::from(has_children);
    v.extend_from_slice(entries);
    v.extend_from_slice(slack);
    v
}

// ------------------------------------------------------------------- image

struct Planned {
    num: u64,
    seq: u16,
    parent: u64,
    name: String,
    kind: Kind,
}

struct Image<'b> {
    b: &'b NtfsBuilder,
    data: Vec<u8>,
    next_lcn: u64,
    used: Vec<bool>,
}

impl<'b> Image<'b> {
    fn new(b: &'b NtfsBuilder) -> Self {
        let size = b.total_clusters as usize * b.cluster_size;
        Self {
            b,
            data: vec![0; size],
            next_lcn: 0,
            used: vec![false; b.total_clusters as usize],
        }
    }

    fn cs(&self) -> u64 {
        self.b.cluster_size as u64
    }

    /// Allocates `n` contiguous clusters, optionally leaving them marked free.
    fn alloc(&mut self, n: u64, mark: bool) -> u64 {
        let lcn = self.next_lcn;
        self.next_lcn += n;
        assert!(
            self.next_lcn < self.b.total_clusters - 1,
            "volume too small"
        );
        if mark {
            for c in lcn..lcn + n {
                self.used[c as usize] = true;
            }
        }
        lcn
    }

    fn write_clusters(&mut self, lcn: u64, bytes: &[u8]) {
        let off = (lcn * self.cs()) as usize;
        put(&mut self.data, off, bytes);
    }

    /// Stores `data` non-resident; returns (runs, alloc size).
    fn store(&mut self, data: &[u8], fragmented: bool, mark: bool) -> (Vec<Run>, u64) {
        let cs = self.cs();
        let n = (data.len() as u64).div_ceil(cs).max(1);
        let runs = if fragmented && n >= 2 {
            let first = n / 2;
            let a = self.alloc(first, mark);
            self.alloc(1, false); // free gap
            let b = self.alloc(n - first, mark);
            vec![
                Run {
                    len: first,
                    lcn: Some(a),
                },
                Run {
                    len: n - first,
                    lcn: Some(b),
                },
            ]
        } else {
            vec![Run {
                len: n,
                lcn: Some(self.alloc(n, mark)),
            }]
        };
        let mut pos = 0usize;
        for r in &runs {
            let take = ((r.len * cs) as usize).min(data.len() - pos);
            let lcn = r.lcn.unwrap_or(0);
            let chunk = data[pos..pos + take].to_vec();
            self.write_clusters(lcn, &chunk);
            pos += take;
        }
        (runs, n * cs)
    }

    /// Stores `data` LZNT1-compressed in 16-cluster units.
    fn store_compressed(&mut self, data: &[u8], mark: bool) -> (Vec<Run>, u64) {
        let cs = self.cs() as usize;
        let unit = 16 * cs;
        let mut runs = Vec::new();
        for chunk in data.chunks(unit) {
            let c = lznt1::compress(chunk);
            let stored = c.len().div_ceil(cs);
            if stored < 16 {
                let lcn = self.alloc(stored as u64, mark);
                self.write_clusters(lcn, &c);
                runs.push(Run {
                    len: stored as u64,
                    lcn: Some(lcn),
                });
                runs.push(Run {
                    len: 16 - stored as u64,
                    lcn: None,
                });
            } else {
                let lcn = self.alloc(16, mark);
                self.write_clusters(lcn, chunk);
                runs.push(Run {
                    len: 16,
                    lcn: Some(lcn),
                });
            }
        }
        let units = data.len().div_ceil(unit).max(1) as u64;
        (merge_runs(runs), units * unit as u64)
    }

    fn finish(mut self) -> Vec<u8> {
        let b = self.b;
        let cs = self.cs();
        let t = DEFAULT_FILETIME;
        let times = [t; 4];

        // ---- plan user records
        let mut by_path: BTreeMap<String, u64> = BTreeMap::new();
        by_path.insert(String::new(), ROOT);
        let mut planned: Vec<Planned> = Vec::new();
        for (i, (path, kind)) in b.nodes.iter().enumerate() {
            let trimmed = path.trim_matches('/');
            let (parent_path, name) = trimmed.rsplit_once('/').unwrap_or(("", trimmed));
            let parent = *by_path
                .get(parent_path)
                .unwrap_or_else(|| panic!("parent of {path} not added"));
            let num = FIRST_USER_RECORD + i as u64;
            by_path.insert(trimmed.to_owned(), num);
            let deleted = matches!(kind, Kind::File(_, o) if o.deleted != Deleted::No);
            planned.push(Planned {
                num,
                seq: if deleted { 2 } else { 1 },
                parent,
                name: name.to_owned(),
                kind: kind.clone(),
            });
        }
        let records = (FIRST_USER_RECORD + planned.len() as u64).next_multiple_of(16);
        let mft_bytes = records * RECORD as u64;

        // ---- fixed layout
        let boot_clusters = 8192u64.div_ceil(cs);
        self.alloc(boot_clusters, true);
        let mirr_clusters = (4 * RECORD as u64).div_ceil(cs);
        let mirr_lcn = self.alloc(mirr_clusters, true);
        let log_lcn = self.alloc(65_536 / cs, true);
        let mft_lcn = self.alloc(mft_bytes.div_ceil(cs), true);

        let mut recs: Vec<Option<Vec<u8>>> = vec![None; records as usize];
        // Index entries per directory: (live entries, slack entries).
        let mut dir_entries: DirEntries = BTreeMap::new();
        let dir_seq = |n: u64| -> u16 { if n == ROOT { 5 } else { 1 } };

        // ---- system files (data first so runs are known)
        let upcase: Vec<u8> = (0u32..65_536)
            .flat_map(|u| {
                upcase_key(&char::from_u32(u).map(String::from).unwrap_or_default())
                    .first()
                    .copied()
                    .unwrap_or(u as u16)
                    .to_le_bytes()
            })
            .collect();
        let (upcase_runs, upcase_alloc) = self.store(&upcase, false, true);
        let attrdef = attrdef_table();
        let (attrdef_runs, attrdef_alloc) = self.store(&attrdef, false, true);
        let bitmap_len = (b.total_clusters.div_ceil(8)).next_multiple_of(8);
        let (bitmap_runs, bitmap_alloc) = self.store(&vec![0u8; bitmap_len as usize], false, true);

        // ---- user files
        for p in &planned {
            let fn_flags;
            let mut attrs: Vec<Vec<u8>> = Vec::new();
            let mut id = 0u16;
            let mut next_id = || {
                id += 1;
                id - 1
            };
            let (ftimes, deleted) = match &p.kind {
                Kind::File(_, o) => (o.times.unwrap_or(times), o.deleted),
                Kind::Dir => (times, Deleted::No),
            };
            let live = deleted == Deleted::No;
            let (real, alloc_sz);
            match &p.kind {
                Kind::Dir => {
                    fn_flags = 0x1000_0000;
                    real = 0;
                    alloc_sz = 0;
                }
                Kind::File(data, o) => {
                    fn_flags = if o.compressed { 0x0800 | 0x20 } else { 0x20 };
                    real = data.len() as u64;
                    let resident = o
                        .resident
                        .unwrap_or(data.len() <= 600 && !o.compressed && !o.fragmented);
                    let si = std_info(ftimes, fn_flags);
                    attrs.push(resident_attr(0x10, None, &si, next_id(), false));
                    // $FILE_NAME added below (needs alloc size); placeholder index.
                    let data_attr = if resident {
                        alloc_sz = data.len() as u64;
                        resident_attr(0x80, None, data, 99, false)
                    } else if o.compressed {
                        let (runs, a) = self.store_compressed(data, live);
                        alloc_sz = a;
                        nonresident_attr(0x80, None, &runs, a, real, 99, 0x0001, 4)
                    } else {
                        let (runs, a) = self.store(data, o.fragmented, live);
                        alloc_sz = a;
                        nonresident_attr(0x80, None, &runs, a, real, 99, 0, 0)
                    };
                    attrs.push(data_attr);
                    for (name, v) in &o.ads {
                        let a = if v.len() <= 300 {
                            resident_attr(0x80, Some(name), v, 99, false)
                        } else {
                            let (runs, al) = self.store(v, false, live);
                            nonresident_attr(0x80, Some(name), &runs, al, v.len() as u64, 99, 0, 0)
                        };
                        attrs.push(a);
                    }
                }
            }
            let parent_seq = if deleted == Deleted::Orphan {
                dir_seq(p.parent) + 7
            } else {
                dir_seq(p.parent)
            };
            let fname = file_name(
                p.parent, parent_seq, &p.name, ftimes, alloc_sz, real, fn_flags,
            );
            if matches!(p.kind, Kind::Dir) {
                attrs.push(resident_attr(
                    0x10,
                    None,
                    &std_info(times, 0x10),
                    next_id(),
                    false,
                ));
            }
            attrs.insert(1, resident_attr(0x30, None, &fname, 0, true));
            // Renumber attribute ids sequentially.
            for (i, a) in attrs.iter_mut().enumerate() {
                put(a, 14, &(i as u16).to_le_bytes());
            }
            match deleted {
                Deleted::No => add_entry(
                    &mut dir_entries,
                    p.parent,
                    p.num,
                    p.seq,
                    &p.name,
                    fname.clone(),
                    false,
                ),
                Deleted::InSlack => add_entry(
                    &mut dir_entries,
                    p.parent,
                    p.num,
                    1,
                    &p.name,
                    fname.clone(),
                    true,
                ),
                Deleted::Unindexed | Deleted::Orphan => {}
            }
            let is_dir = matches!(p.kind, Kind::Dir);
            if is_dir {
                dir_entries.entry(p.num).or_default();
            }
            // Directory index attributes are appended after all entries are known.
            recs[p.num as usize] = Some(encode_partial(attrs, p.seq, live, is_dir));
        }

        // ---- system records' names in the root index
        let sys: [(u64, &str, bool); 12] = [
            (0, "$MFT", false),
            (1, "$MFTMirr", false),
            (2, "$LogFile", false),
            (3, "$Volume", false),
            (4, "$AttrDef", false),
            (5, ".", true),
            (6, "$Bitmap", false),
            (7, "$Boot", false),
            (8, "$BadClus", false),
            (9, "$Secure", false),
            (10, "$UpCase", false),
            (11, "$Extend", true),
        ];
        let sys_sizes: [u64; 12] = [
            mft_bytes,
            4 * RECORD as u64,
            65_536,
            0,
            attrdef.len() as u64,
            0,
            bitmap_len,
            8192,
            0,
            0,
            upcase.len() as u64,
            0,
        ];
        for ((n, name, is_dir), size) in sys.iter().zip(sys_sizes) {
            let seq = if *n == 0 { 1 } else { *n as u16 };
            let flags = if *is_dir { 0x1000_0006 } else { 0x06 };
            add_entry(
                &mut dir_entries,
                ROOT,
                *n,
                seq,
                name,
                file_name(ROOT, 5, name, times, size.next_multiple_of(cs), size, flags),
                false,
            );
        }
        dir_entries.entry(11).or_default();

        // ---- directory indexes (needs cluster allocation for INDX blocks)
        let mut index_attrs: BTreeMap<u64, Vec<Vec<u8>>> = BTreeMap::new();
        let dirs: Vec<(u64, DirSlot)> = std::mem::take(&mut dir_entries).into_iter().collect();
        for (dir, (mut live, slack)) in dirs {
            live.sort_by(|a, b| a.key.cmp(&b.key));
            index_attrs.insert(dir, self.build_index(&live, &slack));
        }

        // ---- system records
        let si = |a: u32| std_info(times, a);
        let fnm = |name: &str, size: u64, flags: u32| {
            file_name(ROOT, 5, name, times, size.next_multiple_of(cs), size, flags)
        };
        let mut mft_bitmap = vec![0u8; (records as usize).div_ceil(8).next_multiple_of(8)];
        let set_bit = |bm: &mut Vec<u8>, n: u64| bm[(n / 8) as usize] |= 1 << (n % 8);
        for n in 0..16 {
            set_bit(&mut mft_bitmap, n);
        }
        for p in &planned {
            if matches!(&p.kind, Kind::Dir)
                || matches!(&p.kind, Kind::File(_, o) if o.deleted == Deleted::No)
            {
                set_bit(&mut mft_bitmap, p.num);
            }
        }
        let sysrec =
            |n: u64, name: &str, size: u64, flags: u32, mut extra: Vec<Vec<u8>>| -> Vec<u8> {
                let mut attrs = vec![
                    resident_attr(0x10, None, &si(flags), 0, false),
                    resident_attr(0x30, None, &fnm(name, size, flags), 1, true),
                ];
                attrs.append(&mut extra);
                for (i, a) in attrs.iter_mut().enumerate() {
                    put(a, 14, &(i as u16).to_le_bytes());
                }
                let seq = if n == 0 { 1 } else { n as u16 };
                let rflags = if index_attrs.contains_key(&n) && (n == ROOT || n == 11) {
                    0x03
                } else {
                    0x01
                };
                mft_record(n, seq, 1, rflags, &attrs)
            };
        let mft_runs = vec![Run {
            len: mft_bytes.div_ceil(cs),
            lcn: Some(mft_lcn),
        }];
        recs[0] = Some(sysrec(
            0,
            "$MFT",
            mft_bytes,
            0x06,
            vec![
                nonresident_attr(
                    0x80,
                    None,
                    &mft_runs,
                    mft_bytes.div_ceil(cs) * cs,
                    mft_bytes,
                    0,
                    0,
                    0,
                ),
                resident_attr(0xB0, None, &mft_bitmap, 0, false),
            ],
        ));
        recs[1] = Some(sysrec(
            1,
            "$MFTMirr",
            4 * RECORD as u64,
            0x06,
            vec![nonresident_attr(
                0x80,
                None,
                &[Run {
                    len: mirr_clusters,
                    lcn: Some(mirr_lcn),
                }],
                mirr_clusters * cs,
                4 * RECORD as u64,
                0,
                0,
                0,
            )],
        ));
        recs[2] = Some(sysrec(
            2,
            "$LogFile",
            65_536,
            0x06,
            vec![nonresident_attr(
                0x80,
                None,
                &[Run {
                    len: 65_536 / cs,
                    lcn: Some(log_lcn),
                }],
                65_536,
                65_536,
                0,
                0,
                0,
            )],
        ));
        let mut volinfo = vec![0u8; 12];
        volinfo[8] = 3;
        volinfo[9] = 1;
        recs[3] = Some(sysrec(
            3,
            "$Volume",
            0,
            0x06,
            vec![
                resident_attr(0x60, None, &utf16(&b.label), 0, false),
                resident_attr(0x70, None, &volinfo, 0, false),
                resident_attr(0x80, None, &[], 0, false),
            ],
        ));
        recs[4] = Some(sysrec(
            4,
            "$AttrDef",
            attrdef.len() as u64,
            0x06,
            vec![nonresident_attr(
                0x80,
                None,
                &attrdef_runs,
                attrdef_alloc,
                attrdef.len() as u64,
                0,
                0,
                0,
            )],
        ));
        recs[5] = Some(sysrec(
            5,
            ".",
            0,
            0x06,
            index_attrs.get(&ROOT).cloned().unwrap_or_default(),
        ));
        recs[6] = Some(sysrec(
            6,
            "$Bitmap",
            bitmap_len,
            0x06,
            vec![nonresident_attr(
                0x80,
                None,
                &bitmap_runs,
                bitmap_alloc,
                bitmap_len,
                0,
                0,
                0,
            )],
        ));
        recs[7] = Some(sysrec(
            7,
            "$Boot",
            8192,
            0x06,
            vec![nonresident_attr(
                0x80,
                None,
                &[Run {
                    len: boot_clusters,
                    lcn: Some(0),
                }],
                boot_clusters * cs,
                8192,
                0,
                0,
                0,
            )],
        ));
        recs[8] = Some(sysrec(
            8,
            "$BadClus",
            0,
            0x06,
            vec![
                resident_attr(0x80, None, &[], 0, false),
                nonresident_attr(
                    0x80,
                    Some("$Bad"),
                    &[Run {
                        len: b.total_clusters - 1,
                        lcn: None,
                    }],
                    (b.total_clusters - 1) * cs,
                    (b.total_clusters - 1) * cs,
                    0,
                    0x8000,
                    0,
                ),
            ],
        ));
        recs[9] = Some(sysrec(
            9,
            "$Secure",
            0,
            0x06,
            vec![resident_attr(0x80, Some("$SDS"), &[], 0, false)],
        ));
        recs[10] = Some(sysrec(
            10,
            "$UpCase",
            upcase.len() as u64,
            0x06,
            vec![nonresident_attr(
                0x80,
                None,
                &upcase_runs,
                upcase_alloc,
                upcase.len() as u64,
                0,
                0,
                0,
            )],
        ));
        recs[11] = Some(sysrec(
            11,
            "$Extend",
            0,
            0x1000_0006,
            index_attrs.get(&11).cloned().unwrap_or_default(),
        ));
        for n in 12..16u64 {
            recs[n as usize] = Some(mft_record(
                n,
                n as u16,
                0,
                0x01,
                &[resident_attr(0x10, None, &si(0x06), 0, false)],
            ));
        }

        // ---- finish user records (append directory index attributes)
        for p in &planned {
            let partial = recs[p.num as usize].take().unwrap_or_default();
            let extra = index_attrs.get(&p.num).cloned().unwrap_or_default();
            recs[p.num as usize] = Some(finish_partial(&partial, p.num, &extra));
        }

        // ---- write MFT, mirror, bitmap, boot sectors
        for (n, r) in recs.iter().enumerate() {
            if let Some(r) = r {
                let off = (mft_lcn * cs) as usize + n * RECORD;
                put(&mut self.data, off, r);
            }
        }
        let mirror: Vec<u8> = (0..4)
            .flat_map(|n| recs[n].clone().unwrap_or_default())
            .collect();
        self.write_clusters(mirr_lcn, &mirror);
        let mut bitmap = vec![0u8; bitmap_len as usize];
        for (c, used) in self.used.iter().enumerate() {
            if *used {
                bitmap[c / 8] |= 1 << (c % 8);
            }
        }
        // Clusters past the end of the volume are marked in use, like Windows.
        for c in (b.total_clusters - 1)..(bitmap_len * 8) {
            bitmap[(c / 8) as usize] |= 1 << (c % 8);
        }
        let bitmap_lcn = bitmap_runs[0].lcn.unwrap_or(0);
        self.write_clusters(bitmap_lcn, &bitmap);

        let boot = self.boot_sector(mft_lcn, mirr_lcn);
        put(&mut self.data, 0, &boot);
        let last = self.data.len() - 512;
        put(&mut self.data, last, &boot);
        self.data
    }

    fn boot_sector(&self, mft_lcn: u64, mirr_lcn: u64) -> Vec<u8> {
        let b = self.b;
        let mut s = vec![0u8; 512];
        put(&mut s, 0, &[0xEB, 0x52, 0x90]);
        put(&mut s, 3, b"NTFS    ");
        put(&mut s, 11, &512u16.to_le_bytes());
        s[13] = (b.cluster_size / 512) as u8;
        s[21] = 0xF8;
        put(&mut s, 24, &63u16.to_le_bytes());
        put(&mut s, 26, &255u16.to_le_bytes());
        put(&mut s, 36, &[0x80, 0x00, 0x80, 0x00]);
        let sectors = b.total_clusters * (b.cluster_size as u64 / 512) - 1;
        put(&mut s, 40, &sectors.to_le_bytes());
        put(&mut s, 48, &mft_lcn.to_le_bytes());
        put(&mut s, 56, &mirr_lcn.to_le_bytes());
        s[64] = (-10i8) as u8; // 1 KiB records
        s[68] = if b.cluster_size >= INDEX_BLOCK {
            (INDEX_BLOCK / b.cluster_size) as u8
        } else {
            (-12i8) as u8
        };
        put(&mut s, 72, &b.serial.to_le_bytes());
        put(&mut s, 510, &[0x55, 0xAA]);
        s
    }

    /// Builds `$INDEX_ROOT` (+ `$INDEX_ALLOCATION`/`$BITMAP`) for a
    /// directory as a bottom-up B-tree of any depth.
    fn build_index(&mut self, live: &[IxEntry], slack: &[IxEntry]) -> Vec<Vec<u8>> {
        const ROOT_BUDGET: usize = 480;
        let cs = self.cs();
        let slack_bytes: Vec<u8> = slack.iter().flat_map(|e| entry_bytes(e, None)).collect();
        let vcn_per_block = (INDEX_BLOCK as u64 / cs).max(1);
        let node_size = |items: &[(IxEntry, Option<u64>)], last: Option<u64>| -> usize {
            items
                .iter()
                .map(|(e, c)| entry_bytes(e, *c).len())
                .sum::<usize>()
                + end_entry(last).len()
        };
        let mut items: Vec<(IxEntry, Option<u64>)> =
            live.iter().map(|e| (e.clone(), None)).collect();
        let mut last_child: Option<u64> = None;
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        let capacity = INDEX_BLOCK - 0x40 - 24 - 64;
        let mut slack_placed = false;

        while node_size(&items, last_child) + slack_bytes.len() * usize::from(!slack_placed) + 32
            > ROOT_BUDGET
        {
            // Split this level into blocks; promote one separator per split.
            let mut promoted: Vec<(IxEntry, Option<u64>)> = Vec::new();
            let mut group: Vec<(IxEntry, Option<u64>)> = Vec::new();
            let mut iter = items.into_iter().peekable();
            while let Some(item) = iter.next() {
                let would = node_size(&group, None) + entry_bytes(&item.0, item.1).len() + 24;
                if would > capacity && !group.is_empty() {
                    // `item` becomes the separator; its old child closes the group.
                    let vcn = blocks.len() as u64 * vcn_per_block;
                    blocks.push(index_block(vcn, &group, item.1, &[]));
                    promoted.push((item.0, Some(vcn)));
                    group = Vec::new();
                    continue;
                }
                group.push(item);
                let _ = iter.peek();
            }
            let vcn = blocks.len() as u64 * vcn_per_block;
            let tail_slack = if slack_placed {
                Vec::new()
            } else {
                slack_bytes.clone()
            };
            slack_placed = true;
            blocks.push(index_block(vcn, &group, last_child, &tail_slack));
            items = promoted;
            last_child = Some(vcn);
        }

        let root_entries: Vec<u8> = items
            .iter()
            .flat_map(|(e, c)| entry_bytes(e, *c))
            .chain(end_entry(last_child))
            .collect();
        let root_slack = if slack_placed {
            Vec::new()
        } else {
            slack_bytes
        };
        let mut root = vec![0u8; 16];
        put(&mut root, 0, &0x30u32.to_le_bytes());
        put(&mut root, 4, &1u32.to_le_bytes());
        put(&mut root, 8, &(INDEX_BLOCK as u32).to_le_bytes());
        root[12] = if cs as usize >= INDEX_BLOCK {
            (INDEX_BLOCK / cs as usize) as u8
        } else {
            (-12i8) as u8
        };
        root.extend_from_slice(&node(
            &root_entries,
            &root_slack,
            last_child.is_some(),
            None,
        ));
        let mut attrs = vec![resident_attr(0x90, Some("$I30"), &root, 0, false)];
        if !blocks.is_empty() {
            let n = blocks.len() as u64;
            let lcn = self.alloc(n * vcn_per_block, true);
            for (k, blk) in blocks.iter().enumerate() {
                self.write_clusters(lcn + k as u64 * vcn_per_block, blk);
            }
            let mut bitmap = vec![0u8; (blocks.len().div_ceil(64)) * 8];
            for k in 0..blocks.len() {
                bitmap[k / 8] |= 1 << (k % 8);
            }
            let alloc = n * INDEX_BLOCK as u64;
            attrs.push(nonresident_attr(
                0xA0,
                Some("$I30"),
                &[Run {
                    len: n * vcn_per_block,
                    lcn: Some(lcn),
                }],
                alloc,
                alloc,
                0,
                0,
                0,
            ));
            attrs.push(resident_attr(0xB0, Some("$I30"), &bitmap, 0, false));
        }
        attrs
    }
}

/// One INDX block: entries (with optional children), end entry, slack.
fn index_block(
    vcn: u64,
    items: &[(IxEntry, Option<u64>)],
    last_child: Option<u64>,
    slack: &[u8],
) -> Vec<u8> {
    let mut blk = vec![0u8; INDEX_BLOCK];
    put(&mut blk, 0, b"INDX");
    put(&mut blk, 16, &vcn.to_le_bytes());
    let entries: Vec<u8> = items
        .iter()
        .flat_map(|(e, c)| entry_bytes(e, *c))
        .chain(end_entry(last_child))
        .collect();
    // Node header at 24; entries start at 0x40 (offset 0x28 from the header).
    let used = 0x28 + entries.len();
    put(&mut blk, 24, &0x28u32.to_le_bytes());
    put(&mut blk, 28, &(used as u32).to_le_bytes());
    put(&mut blk, 32, &((INDEX_BLOCK - 24) as u32).to_le_bytes());
    blk[36] = u8::from(items.iter().any(|(_, c)| c.is_some()) || last_child.is_some());
    put(&mut blk, 0x40, &entries);
    put(&mut blk, 0x40 + entries.len(), slack);
    protect(&mut blk, 40);
    blk
}

/// (live entries, slack entries) of one directory.
type DirSlot = (Vec<IxEntry>, Vec<IxEntry>);
type DirEntries = BTreeMap<u64, DirSlot>;

fn add_entry(
    map: &mut DirEntries,
    dir: u64,
    child: u64,
    seq: u16,
    name: &str,
    fname: Vec<u8>,
    slack: bool,
) {
    let e = IxEntry {
        key: upcase_key(name),
        mref: child | (u64::from(seq) << 48),
        fname,
    };
    let slot = map.entry(dir).or_default();
    if slack {
        slot.1.push(e);
    } else {
        slot.0.push(e);
    }
}

/// Merges adjacent sparse runs.
fn merge_runs(runs: Vec<Run>) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for r in runs {
        match (out.last_mut(), r.lcn) {
            (Some(last), None) if last.lcn.is_none() => last.len += r.len,
            _ => out.push(r),
        }
    }
    out
}

/// Encodes a user record without its index attributes (filled in later).
fn encode_partial(attrs: Vec<Vec<u8>>, seq: u16, live: bool, is_dir: bool) -> Vec<u8> {
    // Stash: [seq(2) flags(2)] + concatenated attrs with u32 length prefix.
    let flags = u16::from(live) | if is_dir { 0x02 } else { 0 };
    let mut v = Vec::new();
    v.extend_from_slice(&seq.to_le_bytes());
    v.extend_from_slice(&flags.to_le_bytes());
    for a in attrs {
        v.extend_from_slice(&(a.len() as u32).to_le_bytes());
        v.extend_from_slice(&a);
    }
    v
}

fn finish_partial(partial: &[u8], num: u64, extra: &[Vec<u8>]) -> Vec<u8> {
    let seq = u16::from_le_bytes([partial[0], partial[1]]);
    let flags = u16::from_le_bytes([partial[2], partial[3]]);
    let mut attrs = Vec::new();
    let mut off = 4;
    while off + 4 <= partial.len() {
        let len = u32::from_le_bytes([
            partial[off],
            partial[off + 1],
            partial[off + 2],
            partial[off + 3],
        ]) as usize;
        attrs.push(partial[off + 4..off + 4 + len].to_vec());
        off += 4 + len;
    }
    let base_id = attrs.len();
    for (i, a) in extra.iter().enumerate() {
        let mut a = a.clone();
        put(&mut a, 14, &((base_id + i) as u16).to_le_bytes());
        attrs.push(a);
    }
    // Attributes must be sorted by type within a record.
    attrs.sort_by_key(|a| u32::from_le_bytes([a[0], a[1], a[2], a[3]]));
    mft_record(num, seq, 1, flags, &attrs)
}

/// Standard `$AttrDef` contents.
fn attrdef_table() -> Vec<u8> {
    let defs: [(&str, u32, u32, u64, u64); 16] = [
        ("$STANDARD_INFORMATION", 0x10, 0x40, 48, 72),
        ("$ATTRIBUTE_LIST", 0x20, 0x80, 0, u64::MAX),
        ("$FILE_NAME", 0x30, 0x42, 68, 578),
        ("$OBJECT_ID", 0x40, 0x40, 0, 256),
        ("$SECURITY_DESCRIPTOR", 0x50, 0x80, 0, u64::MAX),
        ("$VOLUME_NAME", 0x60, 0x40, 2, 256),
        ("$VOLUME_INFORMATION", 0x70, 0x40, 12, 12),
        ("$DATA", 0x80, 0x00, 0, u64::MAX),
        ("$INDEX_ROOT", 0x90, 0x40, 0, u64::MAX),
        ("$INDEX_ALLOCATION", 0xA0, 0x80, 0, u64::MAX),
        ("$BITMAP", 0xB0, 0x80, 0, u64::MAX),
        ("$REPARSE_POINT", 0xC0, 0x80, 0, 16_384),
        ("$EA_INFORMATION", 0xD0, 0x40, 8, 8),
        ("$EA", 0xE0, 0x00, 0, 65_536),
        ("$LOGGED_UTILITY_STREAM", 0x100, 0x80, 0, 65_536),
        ("", 0, 0, 0, 0),
    ];
    let mut out = Vec::new();
    for (name, ty, flags, min, max) in defs {
        let mut e = vec![0u8; 160];
        let n = utf16(name);
        put(&mut e, 0, &n);
        put(&mut e, 128, &ty.to_le_bytes());
        put(&mut e, 136, &u32::from(ty == 0x30).to_le_bytes());
        put(&mut e, 140, &flags.to_le_bytes());
        put(&mut e, 144, &min.to_le_bytes());
        put(&mut e, 152, &max.to_le_bytes());
        out.extend_from_slice(&e);
    }
    out
}

//! Writers for hives and transaction logs (tests, fuzz seeds, fixtures).
//!
//! The output is structurally faithful: 4 KiB hive bins with free-cell
//! padding, `li`/`lf`/`lh`/`ri` subkey lists sorted by upper-cased name,
//! big data (`db`) for values over 16 344 bytes, a shared `sk` cell and a
//! valid base-block checksum. Keys can be written as deleted (free cells).

#![allow(
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test-data writer; sizes are small and controlled"
)]

use crate::base_block::{BASE_BLOCK_SIZE, fix_checksum};
use crate::log::registry_marvin;
use crate::value::ValueType;

/// Default FILETIME for keys: 2021-03-04 10:20:30 UTC.
pub const DEFAULT_FILETIME: u64 = 132_593_268_300_000_000;
const BIN: usize = 4096;
const SEGMENT: usize = 16_344;

/// Which subkey-list cell to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Li,
    Lf,
    Lh,
    /// `ri` over `lh` lists of two entries each.
    Ri,
}

#[derive(Debug, Clone)]
struct BKey {
    name: String,
    children: Vec<usize>,
    values: Vec<(String, u32, Vec<u8>)>,
    last_written: u64,
    class: Option<String>,
    deleted: bool,
}

/// Builds a hive in memory.
#[derive(Debug, Clone)]
pub struct HiveBuilder {
    keys: Vec<BKey>,
    minor: u32,
    list: ListKind,
    seq: (u32, u32),
}

impl HiveBuilder {
    /// A hive whose root key is named `root_name`.
    pub fn new(root_name: &str) -> Self {
        Self {
            keys: vec![BKey {
                name: root_name.to_owned(),
                children: Vec::new(),
                values: Vec::new(),
                last_written: DEFAULT_FILETIME,
                class: None,
                deleted: false,
            }],
            minor: 5,
            list: ListKind::Lh,
            seq: (1, 1),
        }
    }

    /// Format minor version (3 = NT4/2000 style, 5 = XP+).
    #[must_use]
    pub fn minor_version(mut self, v: u32) -> Self {
        self.minor = v;
        self
    }

    /// Subkey-list flavour.
    #[must_use]
    pub fn list_kind(mut self, k: ListKind) -> Self {
        self.list = k;
        self
    }

    /// Base-block sequence numbers (differing = dirty hive).
    #[must_use]
    pub fn sequence(mut self, primary: u32, secondary: u32) -> Self {
        self.seq = (primary, secondary);
        self
    }

    fn find_or_add(&mut self, path: &str, deleted: bool) -> usize {
        let mut cur = 0usize;
        for part in path.split(['\\', '/']).filter(|p| !p.is_empty()) {
            let existing =
                self.keys[cur].children.iter().copied().find(|&c| {
                    self.keys[c].name.eq_ignore_ascii_case(part) && !self.keys[c].deleted
                });
            cur = if let Some(c) = existing {
                c
            } else {
                {
                    self.keys.push(BKey {
                        name: part.to_owned(),
                        children: Vec::new(),
                        values: Vec::new(),
                        last_written: DEFAULT_FILETIME,
                        class: None,
                        deleted: false,
                    });
                    let id = self.keys.len() - 1;
                    self.keys[cur].children.push(id);
                    id
                }
            };
        }
        if deleted {
            self.keys[cur].deleted = true;
        }
        cur
    }

    /// Creates a key (and its parents).
    pub fn key(&mut self, path: &str) -> &mut Self {
        self.find_or_add(path, false);
        self
    }

    /// Sets a key's last-written FILETIME.
    pub fn key_time(&mut self, path: &str, filetime: u64) -> &mut Self {
        let k = self.find_or_add(path, false);
        self.keys[k].last_written = filetime;
        self
    }

    /// Sets a key's class name.
    pub fn class(&mut self, path: &str, class: &str) -> &mut Self {
        let k = self.find_or_add(path, false);
        self.keys[k].class = Some(class.to_owned());
        self
    }

    /// Marks the key at `path` as deleted: its cells are written free and
    /// it is not linked from its parent.
    pub fn delete(&mut self, path: &str) -> &mut Self {
        self.find_or_add(path, true);
        self
    }

    /// Adds (or replaces) a value of any type.
    pub fn value(&mut self, path: &str, name: &str, ty: ValueType, data: &[u8]) -> &mut Self {
        let k = self.find_or_add(path, false);
        let values = &mut self.keys[k].values;
        values.retain(|(n, ..)| !n.eq_ignore_ascii_case(name));
        values.push((name.to_owned(), ty.code(), data.to_vec()));
        self
    }

    /// Adds a `REG_SZ` value.
    pub fn sz(&mut self, path: &str, name: &str, s: &str) -> &mut Self {
        self.value(path, name, ValueType::Sz, &utf16z(s))
    }

    /// Adds a `REG_EXPAND_SZ` value.
    pub fn expand_sz(&mut self, path: &str, name: &str, s: &str) -> &mut Self {
        self.value(path, name, ValueType::ExpandSz, &utf16z(s))
    }

    /// Adds a `REG_MULTI_SZ` value.
    pub fn multi_sz(&mut self, path: &str, name: &str, items: &[&str]) -> &mut Self {
        let mut d = Vec::new();
        for i in items {
            d.extend(utf16z(i));
        }
        d.extend([0, 0]);
        self.value(path, name, ValueType::MultiSz, &d)
    }

    /// Adds a `REG_DWORD` value.
    pub fn dword(&mut self, path: &str, name: &str, v: u32) -> &mut Self {
        self.value(path, name, ValueType::Dword, &v.to_le_bytes())
    }

    /// Adds a `REG_QWORD` value.
    pub fn qword(&mut self, path: &str, name: &str, v: u64) -> &mut Self {
        self.value(path, name, ValueType::Qword, &v.to_le_bytes())
    }

    /// Adds a `REG_BINARY` value.
    pub fn binary(&mut self, path: &str, name: &str, data: &[u8]) -> &mut Self {
        self.value(path, name, ValueType::Binary, data)
    }

    /// Serialises the hive.
    pub fn build(&self) -> Vec<u8> {
        let mut w = Writer::new();
        let root = w.write_key(self, 0, u32::MAX, u32::MAX, true);
        let hbins = w.finish();
        let mut out = vec![0u8; BASE_BLOCK_SIZE];
        out[..4].copy_from_slice(b"regf");
        out[4..8].copy_from_slice(&self.seq.0.to_le_bytes());
        out[8..12].copy_from_slice(&self.seq.1.to_le_bytes());
        out[12..20].copy_from_slice(&DEFAULT_FILETIME.to_le_bytes());
        out[20..24].copy_from_slice(&1u32.to_le_bytes());
        out[24..28].copy_from_slice(&self.minor.to_le_bytes());
        out[32..36].copy_from_slice(&1u32.to_le_bytes());
        out[36..40].copy_from_slice(&root.to_le_bytes());
        out[40..44].copy_from_slice(&(hbins.len() as u32).to_le_bytes());
        out[44..48].copy_from_slice(&1u32.to_le_bytes());
        let fname: Vec<u8> = "\\SystemRoot\\test"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        out[48..48 + fname.len()].copy_from_slice(&fname);
        fix_checksum(&mut out);
        out.extend(hbins);
        out
    }
}

/// UTF-16LE with a terminating NUL.
pub fn utf16z(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
    v.extend([0, 0]);
    v
}

struct Writer {
    data: Vec<u8>,
    bin_start: usize,
    cursor: usize,
}

impl Writer {
    fn new() -> Self {
        let mut w = Self {
            data: Vec::new(),
            bin_start: 0,
            cursor: 0,
        };
        w.open_bin(BIN);
        w
    }

    fn open_bin(&mut self, size: usize) {
        self.bin_start = self.data.len();
        let mut h = vec![0u8; 32];
        h[..4].copy_from_slice(b"hbin");
        h[4..8].copy_from_slice(&(self.bin_start as u32).to_le_bytes());
        h[8..12].copy_from_slice(&(size as u32).to_le_bytes());
        h[20..28].copy_from_slice(&DEFAULT_FILETIME.to_le_bytes());
        self.data.extend(h);
        self.data.resize(self.bin_start + size, 0);
        // The rest of the bin is one free cell until allocated.
        let free = (size - 32) as i32;
        self.data[self.bin_start + 32..self.bin_start + 36].copy_from_slice(&free.to_le_bytes());
        self.cursor = self.bin_start + 32;
    }

    fn bin_end(&self) -> usize {
        let size = u32::from_le_bytes(
            self.data[self.bin_start + 8..self.bin_start + 12]
                .try_into()
                .unwrap(),
        );
        self.bin_start + size as usize
    }

    /// Allocates a cell for `payload` and returns its offset.
    fn alloc(&mut self, payload: &[u8]) -> u32 {
        let len = (payload.len() + 4).next_multiple_of(8);
        if self.cursor + len > self.bin_end() {
            self.close_bin();
            self.open_bin((len + 32).next_multiple_of(BIN));
        }
        let off = self.cursor;
        self.data[off..off + 4].copy_from_slice(&(-(len as i32)).to_le_bytes());
        self.data[off + 4..off + 4 + payload.len()].copy_from_slice(payload);
        self.cursor += len;
        let end = self.bin_end();
        if self.cursor + 8 <= end {
            let free = (end - self.cursor) as i32;
            self.data[self.cursor..self.cursor + 4].copy_from_slice(&free.to_le_bytes());
        }
        off as u32
    }

    fn close_bin(&mut self) {
        let end = self.bin_end();
        if self.cursor < end && end - self.cursor >= 8 {
            let free = (end - self.cursor) as i32;
            self.data[self.cursor..self.cursor + 4].copy_from_slice(&free.to_le_bytes());
        }
    }

    fn patch_u32(&mut self, cell: u32, at: usize, v: u32) {
        let p = cell as usize + 4 + at;
        self.data[p..p + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn free(&mut self, cell: u32) {
        let p = cell as usize;
        let size = i32::from_le_bytes(self.data[p..p + 4].try_into().unwrap());
        self.data[p..p + 4].copy_from_slice(&size.abs().to_le_bytes());
    }

    fn sk(&mut self) -> u32 {
        // Minimal self-relative descriptor: revision 1, no ACLs.
        let sd = [
            1u8, 0, 0x04, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut c = vec![0u8; 20];
        c[..2].copy_from_slice(b"sk");
        c[12..16].copy_from_slice(&1u32.to_le_bytes());
        c[16..20].copy_from_slice(&(sd.len() as u32).to_le_bytes());
        c.extend(sd);
        let off = self.alloc(&c);
        self.patch_u32(off, 4, off);
        self.patch_u32(off, 8, off);
        off
    }

    fn data_cells(&mut self, b: &HiveBuilder, data: &[u8]) -> (u32, u32) {
        if data.len() <= 4 {
            let mut f = [0u8; 4];
            f[..data.len()].copy_from_slice(data);
            return (0x8000_0000 | data.len() as u32, u32::from_le_bytes(f));
        }
        if data.len() > SEGMENT && b.minor >= 4 {
            let segs: Vec<u32> = data.chunks(SEGMENT).map(|s| self.alloc(s)).collect();
            let list: Vec<u8> = segs.iter().flat_map(|s| s.to_le_bytes()).collect();
            let list_off = self.alloc(&list);
            let mut db = vec![0u8; 8];
            db[..2].copy_from_slice(b"db");
            db[2..4].copy_from_slice(&(segs.len() as u16).to_le_bytes());
            db[4..8].copy_from_slice(&list_off.to_le_bytes());
            return (data.len() as u32, self.alloc(&db));
        }
        (data.len() as u32, self.alloc(data))
    }

    fn write_key(&mut self, b: &HiveBuilder, id: usize, parent: u32, sk: u32, root: bool) -> u32 {
        let k = &b.keys[id];
        let (name_bytes, compressed) = encode_name(&k.name);
        let mut nk = vec![0u8; 76];
        nk[..2].copy_from_slice(b"nk");
        let mut flags: u16 = if compressed { 0x20 } else { 0 };
        if root {
            flags |= 0x2C;
        }
        nk[2..4].copy_from_slice(&flags.to_le_bytes());
        nk[4..12].copy_from_slice(&k.last_written.to_le_bytes());
        nk[16..20].copy_from_slice(&parent.to_le_bytes());
        nk[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        nk[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        nk[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        nk[44..48].copy_from_slice(&sk.to_le_bytes());
        nk[48..52].copy_from_slice(&u32::MAX.to_le_bytes());
        nk[72..74].copy_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        nk.extend(&name_bytes);
        let off = self.alloc(&nk);
        // Like Windows, the root key is the first cell and `sk` follows.
        let sk = if sk == u32::MAX {
            let s = self.sk();
            self.patch_u32(off, 44, s);
            s
        } else {
            sk
        };
        let mut freed = Vec::new();

        // Values.
        if !k.values.is_empty() {
            let mut vks = Vec::new();
            for (name, ty, data) in &k.values {
                let (size, data_off) = self.data_cells(b, data);
                let (nb, comp) = encode_name(name);
                let mut vk = vec![0u8; 20];
                vk[..2].copy_from_slice(b"vk");
                vk[2..4].copy_from_slice(&(nb.len() as u16).to_le_bytes());
                vk[4..8].copy_from_slice(&size.to_le_bytes());
                vk[8..12].copy_from_slice(&data_off.to_le_bytes());
                vk[12..16].copy_from_slice(&ty.to_le_bytes());
                vk[16..18].copy_from_slice(&u16::from(comp).to_le_bytes());
                vk.extend(nb);
                let vk_off = self.alloc(&vk);
                if k.deleted {
                    freed.push(vk_off);
                }
                vks.push(vk_off);
            }
            let list: Vec<u8> = vks.iter().flat_map(|v| v.to_le_bytes()).collect();
            let list_off = self.alloc(&list);
            self.patch_u32(off, 36, vks.len() as u32);
            self.patch_u32(off, 40, list_off);
        }

        // Subkeys (deleted ones are written but not linked).
        let mut kids: Vec<usize> = k.children.clone();
        kids.sort_by_key(|&c| b.keys[c].name.to_uppercase());
        let mut linked = Vec::new();
        for c in kids {
            let child_off = self.write_key(b, c, off, sk, false);
            if !b.keys[c].deleted {
                linked.push((child_off, b.keys[c].name.clone()));
            }
        }
        if !linked.is_empty() {
            let list_off = self.subkey_list(b.list, &linked);
            self.patch_u32(off, 20, linked.len() as u32);
            self.patch_u32(off, 28, list_off);
        }
        if let Some(class) = &k.class {
            let cb: Vec<u8> = class.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let c_off = self.alloc(&cb);
            self.patch_u32(off, 48, c_off);
            let p = off as usize + 4 + 74;
            self.data[p..p + 2].copy_from_slice(&(cb.len() as u16).to_le_bytes());
        }
        if k.deleted {
            self.free(off);
            for f in freed {
                self.free(f);
            }
        }
        off
    }

    fn subkey_list(&mut self, kind: ListKind, kids: &[(u32, String)]) -> u32 {
        match kind {
            ListKind::Ri => {
                let subs: Vec<u32> = kids
                    .chunks(2)
                    .map(|c| self.subkey_list(ListKind::Lh, c))
                    .collect();
                let mut ri = Vec::new();
                ri.extend(b"ri");
                ri.extend((subs.len() as u16).to_le_bytes());
                for s in subs {
                    ri.extend(s.to_le_bytes());
                }
                self.alloc(&ri)
            }
            ListKind::Li => {
                let mut c = Vec::new();
                c.extend(b"li");
                c.extend((kids.len() as u16).to_le_bytes());
                for (o, _) in kids {
                    c.extend(o.to_le_bytes());
                }
                self.alloc(&c)
            }
            ListKind::Lf | ListKind::Lh => {
                let mut c = Vec::new();
                c.extend(if kind == ListKind::Lf { b"lf" } else { b"lh" });
                c.extend((kids.len() as u16).to_le_bytes());
                for (o, name) in kids {
                    c.extend(o.to_le_bytes());
                    if kind == ListKind::Lh {
                        c.extend(lh_hash(name).to_le_bytes());
                    } else {
                        let mut h = [0u8; 4];
                        for (i, ch) in name.chars().take(4).enumerate() {
                            h[i] = if ch.is_ascii() { ch as u8 } else { 0 };
                        }
                        c.extend(h);
                    }
                }
                self.alloc(&c)
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.close_bin();
        self.data
    }
}

fn encode_name(name: &str) -> (Vec<u8>, bool) {
    if name.chars().all(|c| (c as u32) < 0x80) {
        (name.bytes().collect(), true)
    } else {
        (
            name.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            false,
        )
    }
}

/// The `lh` name hash.
pub fn lh_hash(name: &str) -> u32 {
    name.to_uppercase()
        .encode_utf16()
        .fold(0u32, |h, c| h.wrapping_mul(37).wrapping_add(u32::from(c)))
}

/// Sets a hive's sequence numbers (and fixes the checksum).
pub fn set_sequence(hive: &mut [u8], primary: u32, secondary: u32) {
    hive[4..8].copy_from_slice(&primary.to_le_bytes());
    hive[8..12].copy_from_slice(&secondary.to_le_bytes());
    fix_checksum(&mut hive[..BASE_BLOCK_SIZE]);
}

/// The 4 KiB pages of the hive bins that differ between two hives.
fn changed_pages(stale: &[u8], updated: &[u8], page: usize) -> Vec<(u32, Vec<u8>)> {
    let bins = &updated[BASE_BLOCK_SIZE..];
    let old = stale.get(BASE_BLOCK_SIZE..).unwrap_or_default();
    bins.chunks(page)
        .enumerate()
        .filter(|(i, p)| old.get(i * page..i * page + p.len()) != Some(*p))
        .map(|(i, p)| ((i * page) as u32, p.to_vec()))
        .collect()
}

/// A new-format (`HvLE`) log that turns `stale` into `updated`. Entries
/// get sequence numbers from `first_seq`, `pages_per_entry` dirty pages
/// each.
pub fn new_format_log(
    stale: &[u8],
    updated: &[u8],
    first_seq: u32,
    pages_per_entry: usize,
) -> Vec<u8> {
    let mut log = updated[..BASE_BLOCK_SIZE].to_vec();
    log[28..32].copy_from_slice(&6u32.to_le_bytes());
    log[4..8].copy_from_slice(&first_seq.to_le_bytes());
    log[8..12].copy_from_slice(&first_seq.to_le_bytes());
    fix_checksum(&mut log);
    log.truncate(512);
    let hbins = (updated.len() - BASE_BLOCK_SIZE) as u32;
    let pages = changed_pages(stale, updated, BIN);
    for (n, group) in pages.chunks(pages_per_entry.max(1)).enumerate() {
        let mut e = vec![0u8; 40];
        e[..4].copy_from_slice(b"HvLE");
        e[12..16].copy_from_slice(&(first_seq + n as u32).to_le_bytes());
        e[16..20].copy_from_slice(&hbins.to_le_bytes());
        e[20..24].copy_from_slice(&(group.len() as u32).to_le_bytes());
        for (off, p) in group {
            e.extend(off.to_le_bytes());
            e.extend((p.len() as u32).to_le_bytes());
        }
        for (_, p) in group {
            e.extend(p);
        }
        e.resize(e.len().next_multiple_of(512), 0);
        let len = e.len() as u32;
        e[4..8].copy_from_slice(&len.to_le_bytes());
        let h1 = registry_marvin(&e[40..]);
        e[24..32].copy_from_slice(&h1.to_le_bytes());
        let h2 = registry_marvin(&e[..32]);
        e[32..40].copy_from_slice(&h2.to_le_bytes());
        log.extend(e);
    }
    log
}

/// An old-format (`DIRT`) log that turns `stale` into `updated`.
pub fn old_format_log(stale: &[u8], updated: &[u8]) -> Vec<u8> {
    let mut log = updated[..BASE_BLOCK_SIZE].to_vec();
    log[28..32].copy_from_slice(&1u32.to_le_bytes());
    fix_checksum(&mut log);
    log.truncate(512);
    let hbins = updated.len() - BASE_BLOCK_SIZE;
    let sectors = hbins.div_ceil(512);
    let mut bitmap = vec![0u8; sectors.div_ceil(8)];
    let dirty = changed_pages(stale, updated, 512);
    for (off, _) in &dirty {
        let s = *off as usize / 512;
        bitmap[s / 8] |= 1 << (s % 8);
    }
    log.extend(b"DIRT");
    log.extend(&bitmap);
    log.resize(log.len().next_multiple_of(512), 0);
    for (_, p) in dirty {
        let mut p = p;
        p.resize(512, 0);
        log.extend(p);
    }
    log
}

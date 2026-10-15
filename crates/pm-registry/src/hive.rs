//! Hive, key and value access.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;

use jiff::Timestamp;
use pm_core::cache::ShardedLru;
use pm_core::decode::utf16le_lossy;
use pm_core::{Bytes, SharedSource};

use crate::base_block::{BASE_BLOCK_SIZE, BaseBlock};
use crate::data::HiveData;
use crate::log::{ReplayReport, replay};
use crate::value::{ValueType, decode_multi_sz, decode_sz};
use crate::{Error, Result};

/// Largest cell accepted (corrupt size fields must not allocate gigabytes).
const MAX_CELL: usize = 64 << 20;
/// Cells up to this size are kept in the LRU cache.
const CACHE_MAX_CELL: usize = 4 << 10;
/// Most entries collected from one subkey or value list.
const MAX_LIST: usize = 1 << 20;
/// Nesting limit for `ri` index lists.
const MAX_RI_DEPTH: u32 = 8;
/// Parent-chain limit when building paths.
const MAX_DEPTH: usize = 512;
/// Payload bytes of one big-data segment.
const BIG_DATA_SEGMENT: usize = 16_344;

/// `nk` flag: the key is the hive's root.
const KEY_HIVE_ENTRY: u16 = 0x0004;
/// `nk` flag: the name is stored as 8-bit characters.
const KEY_COMP_NAME: u16 = 0x0020;
/// `vk` flag: the name is stored as 8-bit characters.
const VALUE_COMP_NAME: u16 = 0x0001;

/// An open registry hive.
#[derive(Debug)]
pub struct Hive {
    data: HiveData,
    header: BaseBlock,
    root_offset: u32,
    cache: ShardedLru<u32, Arc<[u8]>>,
    replay: ReplayReport,
}

impl Hive {
    /// Opens a hive without transaction logs.
    pub fn open(src: SharedSource) -> Result<Self> {
        Self::open_with_logs(src, &[])
    }

    /// Opens a hive and, if it is dirty, replays its transaction logs
    /// (`.LOG`, `.LOG1`, `.LOG2`; any order, invalid logs are ignored).
    pub fn open_with_logs(src: SharedSource, logs: &[SharedSource]) -> Result<Self> {
        let size = src.size();
        if size < 512 {
            return Err(Error::NotRegf);
        }
        let mut base = src.read_vec_at(0, BASE_BLOCK_SIZE.min(size as usize))?;
        base.resize(BASE_BLOCK_SIZE, 0);
        let first = BaseBlock::parse(&base)?;
        let mut data = HiveData::new(src, base);
        let replay_report = if first.is_dirty() && !logs.is_empty() {
            replay(&mut data, &first, logs)
        } else {
            ReplayReport::default()
        };
        let header = BaseBlock::parse(data.base())?;
        let mut hive = Self {
            data,
            root_offset: header.root_offset,
            header,
            cache: ShardedLru::new(16, 256),
            replay: replay_report,
        };
        if hive.root().is_err() {
            // Damaged header: the root key is almost always the first cell.
            hive.root_offset = 0x20;
            hive.root()?;
        }
        Ok(hive)
    }

    /// The base block (after any log replay).
    pub fn header(&self) -> &BaseBlock {
        &self.header
    }

    /// What transaction-log replay did.
    pub fn replay_report(&self) -> &ReplayReport {
        &self.replay
    }

    /// Size of the hive bins area as stored (or grown by log replay).
    pub(crate) fn data_len(&self) -> u64 {
        self.data.len().saturating_sub(BASE_BLOCK_SIZE as u64)
    }

    /// Reads raw bytes of the hive bins area.
    pub(crate) fn read_bins(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        Ok(self.data.read(BASE_BLOCK_SIZE as u64 + offset, len)?)
    }

    /// The root key.
    pub fn root(&self) -> Result<Key<'_>> {
        Key::load(self, self.root_offset)
    }

    /// Looks up a key by path relative to the root (`\` or `/` separated,
    /// case-insensitive). `""` is the root.
    pub fn key(&self, path: &str) -> Result<Option<Key<'_>>> {
        self.root()?.subkey_path(path)
    }

    /// Visits keys depth-first (each key once, at most `max_keys`), letting
    /// `f` stop early. Unreadable subtrees are skipped.
    pub fn walk(&self, max_keys: usize, mut f: impl FnMut(&Key<'_>) -> ControlFlow<()>) {
        let Ok(root) = self.root() else { return };
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(k) = stack.pop() {
            if seen.len() >= max_keys || !seen.insert(k.offset) {
                continue;
            }
            if f(&k).is_break() {
                return;
            }
            if let Ok(mut subs) = k.subkeys() {
                subs.reverse();
                stack.extend(subs);
            }
        }
    }

    /// The payload of the cell at `offset` (relative to the hive bins),
    /// without its size field.
    pub(crate) fn cell(&self, offset: u32) -> Result<Arc<[u8]>> {
        if let Some(c) = self.cache.get(&offset) {
            return Ok(c);
        }
        let (_, payload) = self.raw_cell(offset)?;
        let payload: Arc<[u8]> = payload.into();
        if payload.len() <= CACHE_MAX_CELL {
            self.cache.insert(offset, Arc::clone(&payload));
        }
        Ok(payload)
    }

    /// Size field (negative = allocated) and payload of a cell.
    pub(crate) fn raw_cell(&self, offset: u32) -> Result<(i32, Vec<u8>)> {
        let bad = |reason| Error::BadCell { offset, reason };
        if offset == u32::MAX || u64::from(offset) >= self.data_len() {
            return Err(bad("offset outside the hive bins"));
        }
        let head = self.read_bins(u64::from(offset), 4)?;
        let size = head.i32_le(0)?;
        let len = size.unsigned_abs() as usize;
        if len < 8 {
            return Err(bad("cell too small"));
        }
        if len > MAX_CELL {
            return Err(bad("cell too large"));
        }
        let payload = self
            .read_bins(u64::from(offset) + 4, len - 4)
            .map_err(|_| bad("cell runs past the hive end"))?;
        Ok((size, payload))
    }

    /// Hive format minor version (big data needs ≥ 4).
    fn minor(&self) -> u32 {
        self.header.minor_version
    }
}

/// A registry key (`nk` cell).
#[derive(Debug, Clone)]
pub struct Key<'h> {
    hive: &'h Hive,
    offset: u32,
    cell: Arc<[u8]>,
}

/// A subkey-list hint used to skip non-matching keys during lookups.
#[derive(Debug, Clone, Copy)]
enum Hint {
    None,
    /// `lf`: the first four name characters.
    Prefix([u8; 4]),
    /// `lh`: the name hash.
    Hash(u32),
}

impl<'h> Key<'h> {
    pub(crate) fn load(hive: &'h Hive, offset: u32) -> Result<Self> {
        let cell = hive.cell(offset)?;
        if cell.get(..2) != Some(b"nk") || cell.len() < 76 {
            return Err(Error::BadCell {
                offset,
                reason: "not a key (nk) cell",
            });
        }
        Ok(Self { hive, offset, cell })
    }

    fn u16(&self, at: usize) -> u16 {
        self.cell.u16_le(at).unwrap_or(0)
    }

    fn u32(&self, at: usize) -> u32 {
        self.cell.u32_le(at).unwrap_or(0)
    }

    /// Cell offset (stable identity within the hive).
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// The hive this key belongs to.
    pub fn hive(&self) -> &'h Hive {
        self.hive
    }

    /// `nk` flags.
    pub fn flags(&self) -> u16 {
        self.u16(2)
    }

    /// Whether this is the hive's root key.
    pub fn is_root(&self) -> bool {
        self.flags() & KEY_HIVE_ENTRY != 0 || self.offset == self.hive.root_offset
    }

    /// The key name.
    pub fn name(&self) -> String {
        let len = usize::from(self.u16(72));
        let raw = self
            .cell
            .get(76..)
            .map(|r| r.get(..len).unwrap_or(r))
            .unwrap_or_default();
        decode_name(raw, self.flags() & KEY_COMP_NAME != 0)
    }

    /// Last-written time as a raw FILETIME.
    pub fn last_written_filetime(&self) -> u64 {
        self.cell.u64_le(4).unwrap_or(0)
    }

    /// Last-written time.
    pub fn last_written(&self) -> Option<Timestamp> {
        pm_core::time::from_filetime(self.last_written_filetime())
    }

    /// Last-written time as Unix seconds.
    pub fn last_written_unix(&self) -> Option<i64> {
        self.last_written().map(Timestamp::as_second)
    }

    /// Number of (stable) subkeys recorded in the key.
    pub fn subkey_count(&self) -> u32 {
        self.u32(20)
    }

    /// Number of values recorded in the key.
    pub fn value_count(&self) -> u32 {
        self.u32(36)
    }

    /// The class name, if any.
    pub fn class_name(&self) -> Option<String> {
        let off = self.u32(48);
        let len = usize::from(self.u16(74));
        if off == u32::MAX || len == 0 {
            return None;
        }
        let c = self.hive.cell(off).ok()?;
        Some(utf16le_lossy(c.get(..len).unwrap_or(&c)))
    }

    /// Raw security descriptor from the key's `sk` cell.
    pub fn security_descriptor(&self) -> Option<Vec<u8>> {
        let c = self.hive.cell(self.u32(44)).ok()?;
        if c.get(..2) != Some(b"sk") {
            return None;
        }
        let len = c.u32_le(16).ok()? as usize;
        c.get(20..20 + len).map(<[u8]>::to_vec)
    }

    /// The parent key (`None` for the root or a broken link).
    pub fn parent(&self) -> Option<Key<'h>> {
        if self.is_root() {
            return None;
        }
        Key::load(self.hive, self.u32(16)).ok()
    }

    /// Path from the root (root name excluded), `\` separated.
    pub fn path(&self) -> String {
        let mut names = Vec::new();
        let mut cur = self.clone();
        let mut seen = HashSet::new();
        while !cur.is_root() && names.len() < MAX_DEPTH && seen.insert(cur.offset) {
            names.push(cur.name());
            match cur.parent() {
                Some(p) => cur = p,
                None => break,
            }
        }
        names.reverse();
        names.join("\\")
    }

    /// All subkeys; unreadable entries are skipped.
    pub fn subkeys(&self) -> Result<Vec<Key<'h>>> {
        if self.subkey_count() == 0 {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        collect_list(self.hive, self.u32(28), 0, &mut entries)?;
        Ok(entries
            .into_iter()
            .filter_map(|(off, _)| Key::load(self.hive, off).ok())
            .collect())
    }

    /// The subkey named `name` (case-insensitive).
    pub fn subkey(&self, name: &str) -> Result<Option<Key<'h>>> {
        if self.subkey_count() == 0 {
            return Ok(None);
        }
        let mut entries = Vec::new();
        collect_list(self.hive, self.u32(28), 0, &mut entries)?;
        let upper: Vec<u16> = name.to_uppercase().encode_utf16().collect();
        let hash = upper
            .iter()
            .fold(0u32, |h, &c| h.wrapping_mul(37).wrapping_add(u32::from(c)));
        let prefix = lf_hint(name);
        let hinted = |h: &Hint| match *h {
            Hint::None => true,
            Hint::Hash(x) => x == hash,
            Hint::Prefix(p) => prefix.is_none_or(|want| want.eq_ignore_ascii_case(&p)),
        };
        // Hints first; tools that write bogus hints fall back to a scan.
        for pass in [true, false] {
            for (off, hint) in &entries {
                if pass != hinted(hint) {
                    continue;
                }
                if let Ok(k) = Key::load(self.hive, *off)
                    && names_equal(&k.name(), name)
                {
                    return Ok(Some(k));
                }
            }
        }
        Ok(None)
    }

    /// Follows a relative path of subkeys (`\` or `/` separated).
    pub fn subkey_path(&self, path: &str) -> Result<Option<Key<'h>>> {
        let mut cur = self.clone();
        for part in path.split(['\\', '/']).filter(|p| !p.is_empty()) {
            match cur.subkey(part)? {
                Some(k) => cur = k,
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    /// All values; unreadable entries are skipped.
    pub fn values(&self) -> Result<Vec<Value<'h>>> {
        let count = (self.value_count() as usize).min(MAX_LIST);
        if count == 0 {
            return Ok(Vec::new());
        }
        let list = self.hive.cell(self.u32(40))?;
        Ok(list
            .as_chunks::<4>()
            .0
            .iter()
            .take(count)
            .filter_map(|c| Value::load(self.hive, u32::from_le_bytes(*c)).ok())
            .collect())
    }

    /// The value named `name` (case-insensitive; `""` is the default value).
    pub fn value(&self, name: &str) -> Result<Option<Value<'h>>> {
        Ok(self
            .values()?
            .into_iter()
            .find(|v| names_equal(&v.name(), name)))
    }

    /// Convenience: a value's data as a string (see [`Value::string`]).
    pub fn string_value(&self, name: &str) -> Option<String> {
        self.value(name).ok().flatten().and_then(|v| v.string())
    }

    /// Convenience: a value's data as a DWORD (see [`Value::dword`]).
    pub fn dword_value(&self, name: &str) -> Option<u32> {
        self.value(name).ok().flatten().and_then(|v| v.dword())
    }

    /// Convenience: a value's raw data.
    pub fn binary_value(&self, name: &str) -> Option<Vec<u8>> {
        self.value(name).ok().flatten().and_then(|v| v.data().ok())
    }
}

/// Collects `(cell offset, hint)` entries of a subkey list.
fn collect_list(hive: &Hive, offset: u32, depth: u32, out: &mut Vec<(u32, Hint)>) -> Result<()> {
    if depth > MAX_RI_DEPTH {
        return Err(Error::BadCell {
            offset,
            reason: "index lists nested too deeply",
        });
    }
    let c = hive.cell(offset)?;
    let sig = c.get(..2).unwrap_or_default();
    let count = usize::from(c.u16_le(2)?);
    let body = c.get(4..).unwrap_or_default();
    match sig {
        b"li" => {
            for e in body.as_chunks::<4>().0.iter().take(count) {
                push(out, u32::from_le_bytes(*e), Hint::None);
            }
        }
        b"lf" | b"lh" => {
            for e in body.as_chunks::<8>().0.iter().take(count) {
                let (off, hint) = e.split_at(4);
                let off = u32::from_le_bytes(off.try_into().unwrap_or_default());
                let hint: [u8; 4] = hint.try_into().unwrap_or_default();
                let h = if sig == b"lh" {
                    Hint::Hash(u32::from_le_bytes(hint))
                } else {
                    Hint::Prefix(hint)
                };
                push(out, off, h);
            }
        }
        b"ri" => {
            for e in body.as_chunks::<4>().0.iter().take(count) {
                // A damaged sub-list should not hide its siblings.
                if let Err(e) = collect_list(hive, u32::from_le_bytes(*e), depth + 1, out) {
                    tracing::debug!(%e, "skipping damaged index sub-list");
                }
            }
        }
        _ => {
            return Err(Error::BadCell {
                offset,
                reason: "not a subkey list",
            });
        }
    }
    Ok(())
}

fn push(out: &mut Vec<(u32, Hint)>, off: u32, h: Hint) {
    if out.len() < MAX_LIST {
        out.push((off, h));
    }
}

/// The `lf` hint of a name: its first four characters when they are all
/// ASCII (non-ASCII hints are not compared).
fn lf_hint(name: &str) -> Option<[u8; 4]> {
    let mut h = [0u8; 4];
    for (i, c) in name.chars().take(4).enumerate() {
        if !c.is_ascii() {
            return None;
        }
        if let Some(slot) = h.get_mut(i) {
            *slot = c as u8;
        }
    }
    Some(h)
}

fn decode_name(raw: &[u8], compressed: bool) -> String {
    if compressed {
        raw.iter().map(|&b| char::from(b)).collect()
    } else {
        utf16le_lossy(raw)
    }
}

/// Case-insensitive name comparison (Unicode upper-casing, as Windows).
pub(crate) fn names_equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
        || ((!a.is_ascii() || !b.is_ascii()) && a.to_uppercase() == b.to_uppercase())
}

/// A registry value (`vk` cell).
#[derive(Debug, Clone)]
pub struct Value<'h> {
    hive: &'h Hive,
    offset: u32,
    cell: Arc<[u8]>,
}

impl<'h> Value<'h> {
    pub(crate) fn load(hive: &'h Hive, offset: u32) -> Result<Self> {
        let cell = hive.cell(offset)?;
        if cell.get(..2) != Some(b"vk") || cell.len() < 20 {
            return Err(Error::BadCell {
                offset,
                reason: "not a value (vk) cell",
            });
        }
        Ok(Self { hive, offset, cell })
    }

    /// Cell offset.
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// The value name (`""` for the default value).
    pub fn name(&self) -> String {
        let len = usize::from(self.cell.u16_le(2).unwrap_or(0));
        let flags = self.cell.u16_le(16).unwrap_or(0);
        let raw = self
            .cell
            .get(20..)
            .map(|r| r.get(..len).unwrap_or(r))
            .unwrap_or_default();
        decode_name(raw, flags & VALUE_COMP_NAME != 0)
    }

    /// The data type.
    pub fn data_type(&self) -> ValueType {
        ValueType::from_code(self.cell.u32_le(12).unwrap_or(0))
    }

    /// Declared data size in bytes.
    pub fn data_size(&self) -> u32 {
        self.cell.u32_le(4).unwrap_or(0) & 0x7FFF_FFFF
    }

    /// The raw data (big data reassembled; truncated cells yield what is
    /// there).
    pub fn data(&self) -> Result<Vec<u8>> {
        let raw = self.cell.u32_le(4)?;
        let size = (raw & 0x7FFF_FFFF) as usize;
        let field = self.cell.array::<4>(8)?;
        if raw & 0x8000_0000 != 0 {
            return Ok(field.get(..size.min(4)).unwrap_or_default().to_vec());
        }
        if size == 0 {
            return Ok(Vec::new());
        }
        if size > MAX_CELL {
            return Err(Error::BadCell {
                offset: self.offset,
                reason: "value data too large",
            });
        }
        let data_off = u32::from_le_bytes(field);
        let c = self.hive.cell(data_off)?;
        if size > BIG_DATA_SEGMENT && self.hive.minor() >= 4 && c.get(..2) == Some(b"db") {
            return big_data(self.hive, &c, size);
        }
        Ok(c.get(..size).unwrap_or(&c).to_vec())
    }

    /// Text of a `REG_SZ`/`REG_EXPAND_SZ`/`REG_LINK` value, or the
    /// strings of a `REG_MULTI_SZ` joined with `", "`.
    pub fn string(&self) -> Option<String> {
        let t = self.data_type();
        let data = self.data().ok()?;
        if t.is_string() {
            Some(decode_sz(&data))
        } else if t == ValueType::MultiSz {
            Some(decode_multi_sz(&data).join(", "))
        } else {
            None
        }
    }

    /// The strings of a `REG_MULTI_SZ` value (a string value yields one).
    pub fn multi_string(&self) -> Option<Vec<String>> {
        let t = self.data_type();
        let data = self.data().ok()?;
        if t == ValueType::MultiSz {
            Some(decode_multi_sz(&data))
        } else if t.is_string() {
            Some(vec![decode_sz(&data)])
        } else {
            None
        }
    }

    /// A DWORD (`REG_DWORD`, `REG_DWORD_BIG_ENDIAN`, or 4-byte binary).
    pub fn dword(&self) -> Option<u32> {
        let data = self.data().ok()?;
        let b = data.array::<4>(0).ok()?;
        match self.data_type() {
            ValueType::DwordBigEndian => Some(u32::from_be_bytes(b)),
            ValueType::Dword | ValueType::Binary | ValueType::None | ValueType::Qword => {
                Some(u32::from_le_bytes(b))
            }
            _ => None,
        }
    }

    /// A QWORD (`REG_QWORD` or 8-byte binary), or a widened DWORD.
    pub fn qword(&self) -> Option<u64> {
        let data = self.data().ok()?;
        match self.data_type() {
            ValueType::Qword | ValueType::Binary | ValueType::None if data.len() >= 8 => {
                data.u64_le(0).ok()
            }
            _ => self.dword().map(u64::from),
        }
    }
}

fn big_data(hive: &Hive, db: &[u8], size: usize) -> Result<Vec<u8>> {
    let count = usize::from(db.u16_le(2)?);
    let list = hive.cell(db.u32_le(4)?)?;
    let mut out = Vec::with_capacity(size.min(MAX_CELL));
    for seg in list.as_chunks::<4>().0.iter().take(count) {
        if out.len() >= size {
            break;
        }
        let c = hive.cell(u32::from_le_bytes(*seg))?;
        let take = (size - out.len()).min(BIG_DATA_SEGMENT).min(c.len());
        out.extend_from_slice(c.get(..take).unwrap_or_default());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_comparison() {
        assert!(names_equal("ControlSet001", "CONTROLSET001"));
        assert!(names_equal("Äpfel", "äPFEL"));
        assert!(!names_equal("a", "b"));
    }

    #[test]
    fn hints() {
        assert_eq!(lf_hint("Software"), Some(*b"Soft"));
        assert_eq!(lf_hint("ab"), Some([b'a', b'b', 0, 0]));
        assert_eq!(lf_hint("Äb"), None);
    }
}

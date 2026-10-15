//! Recovery of deleted keys and values from free cells.
//!
//! Deleting a key frees its `nk` cell (the size field turns positive) but
//! leaves the bytes in place until the space is reused. The scan walks the
//! hive bins one bin at a time, so memory stays bounded by the largest bin.

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;

use crate::hive::Hive;
use crate::value::ValueType;

/// Largest hbin accepted while scanning.
const MAX_BIN: u32 = 64 << 20;

/// A key found in a free cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedKey {
    /// Cell offset.
    pub offset: u32,
    pub name: String,
    /// FILETIME.
    pub last_written: u64,
    /// Offset of the parent key cell (may itself be gone).
    pub parent_offset: u32,
    /// Path of the parent when it is still an allocated key.
    pub parent_path: Option<String>,
    /// Values whose cells are still intact.
    pub values: Vec<DeletedValue>,
}

/// A value of a deleted key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedValue {
    pub name: String,
    pub data_type: ValueType,
    /// Data, when it could be read back (inline or a cell of the right size).
    pub data: Option<Vec<u8>>,
}

impl Hive {
    /// Keys recoverable from free cells (at most `max`).
    pub fn deleted_keys(&self, max: usize) -> Vec<DeletedKey> {
        let mut out = Vec::new();
        let end = self.data_len();
        let mut bin_at = 0u64;
        while bin_at + 32 <= end && out.len() < max {
            let Ok(head) = self.read_bins(bin_at, 32) else {
                break;
            };
            if head.get(..4) != Some(b"hbin") {
                break;
            }
            let size = head.u32_le(8).unwrap_or(0);
            if !(4096..=MAX_BIN).contains(&size) || !size.is_multiple_of(4096) {
                break;
            }
            if let Ok(bin) = self.read_bins(bin_at, size as usize) {
                self.scan_bin(&bin, bin_at as u32, max, &mut out);
            }
            bin_at += u64::from(size);
        }
        out
    }

    fn scan_bin(&self, bin: &[u8], bin_off: u32, max: usize, out: &mut Vec<DeletedKey>) {
        let mut at = 32usize;
        while at + 8 <= bin.len() && out.len() < max {
            let Ok(size) = bin.i32_le(at) else { break };
            let len = size.unsigned_abs() as usize;
            if len < 8 || at + len > bin.len() {
                break;
            }
            if size > 0
                && let Some(cell) = bin.get(at + 4..at + len)
                && cell.get(..2) == Some(b"nk")
                && let Some(k) = self.recover_key(cell, bin_off + at as u32)
            {
                out.push(k);
            }
            at += len;
        }
    }

    fn recover_key(&self, c: &[u8], offset: u32) -> Option<DeletedKey> {
        let flags = c.u16_le(2).ok()?;
        let name_len = usize::from(c.u16_le(72).ok()?);
        let raw = c.get(76..76 + name_len)?;
        let name = if flags & 0x20 != 0 {
            raw.iter().map(|&b| char::from(b)).collect()
        } else {
            utf16le_lossy(raw)
        };
        if name.is_empty() {
            return None;
        }
        let parent_offset = c.u32_le(16).ok()?;
        let parent_path = crate::hive::Key::load(self, parent_offset)
            .ok()
            .map(|p| p.path());
        let value_count = (c.u32_le(36).ok()? as usize).min(4096);
        let mut values = Vec::new();
        if value_count > 0
            && let Ok((_, list)) = self.raw_cell(c.u32_le(40).ok()?)
        {
            for e in list.as_chunks::<4>().0.iter().take(value_count) {
                if let Some(v) = self.recover_value(u32::from_le_bytes(*e)) {
                    values.push(v);
                }
            }
        }
        Some(DeletedKey {
            offset,
            name,
            last_written: c.u64_le(4).ok()?,
            parent_offset,
            parent_path,
            values,
        })
    }

    fn recover_value(&self, offset: u32) -> Option<DeletedValue> {
        let (_, c) = self.raw_cell(offset).ok()?;
        if c.get(..2) != Some(b"vk") {
            return None;
        }
        let name_len = usize::from(c.u16_le(2).ok()?);
        let flags = c.u16_le(16).ok()?;
        let raw = c.get(20..20 + name_len)?;
        let name = if flags & 1 != 0 {
            raw.iter().map(|&b| char::from(b)).collect()
        } else {
            utf16le_lossy(raw)
        };
        let raw_size = c.u32_le(4).ok()?;
        let size = (raw_size & 0x7FFF_FFFF) as usize;
        let field = c.array::<4>(8).ok()?;
        let data = if raw_size & 0x8000_0000 != 0 {
            field.get(..size.min(4)).map(<[u8]>::to_vec)
        } else if size == 0 {
            Some(Vec::new())
        } else {
            self.raw_cell(u32::from_le_bytes(field))
                .ok()
                .filter(|(_, d)| d.len() >= size && size <= 16_344)
                .and_then(|(_, d)| d.get(..size).map(<[u8]>::to_vec))
        };
        Some(DeletedValue {
            name,
            data_type: ValueType::from_code(c.u32_le(12).ok()?),
            data,
        })
    }
}

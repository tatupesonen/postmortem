//! GUID Partition Table.

use pm_core::decode::utf16le_lossy;
use pm_core::{Bytes, ReadAt};
use uuid::Uuid;

use crate::types::gpt_type_name;
use crate::{Partition, PartitionType, SlotKind, VolumeError, VolumeSystem, VsKind, layout};

/// Refuse partition arrays larger than this (corrupt/hostile headers).
const MAX_ENTRY_ARRAY_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
struct Header {
    my_lba: u64,
    disk_guid: Uuid,
    entries_lba: u64,
    num_entries: u32,
    entry_size: u32,
    entries_crc: u32,
}

/// Reads and validates (signature, size, CRC) a GPT header at `lba`.
fn read_header(src: &dyn ReadAt, lba: u64, ss: u32) -> Result<Option<Header>, String> {
    let Some(off) = lba.checked_mul(u64::from(ss)) else {
        return Err("header LBA overflows".into());
    };
    let Ok(buf) = src.read_vec_at(off, ss as usize) else {
        return Ok(None);
    };
    if buf.get(..8) != Some(b"EFI PART".as_slice()) {
        return Ok(None);
    }
    let hdr_size = buf.u32_le(12).map_err(|e| e.to_string())?;
    if !(92..=ss).contains(&hdr_size) {
        return Err(format!("header size {hdr_size} out of range"));
    }
    let stored_crc = buf.u32_le(16).map_err(|e| e.to_string())?;
    let mut h = buf
        .sub(0, hdr_size as usize)
        .map_err(|e| e.to_string())?
        .to_vec();
    if let Some(crc) = h.get_mut(16..20) {
        crc.fill(0);
    }
    if crc32fast::hash(&h) != stored_crc {
        return Err(format!("header CRC mismatch at LBA {lba}"));
    }
    let f = |r: Result<u64, pm_core::Truncated>| r.map_err(|e| e.to_string());
    let header = Header {
        my_lba: f(buf.u64_le(24))?,
        disk_guid: Uuid::from_bytes_le(buf.array(56).map_err(|e| e.to_string())?),
        entries_lba: f(buf.u64_le(72))?,
        num_entries: buf.u32_le(80).map_err(|e| e.to_string())?,
        entry_size: buf.u32_le(84).map_err(|e| e.to_string())?,
        entries_crc: buf.u32_le(88).map_err(|e| e.to_string())?,
    };
    if header.my_lba != lba {
        return Err(format!(
            "header claims LBA {} but was read from {lba}",
            header.my_lba
        ));
    }
    if header.entry_size < 128 || !header.entry_size.is_multiple_of(8) {
        return Err(format!("invalid entry size {}", header.entry_size));
    }
    Ok(Some(header))
}

fn read_entries(src: &dyn ReadAt, h: &Header, ss: u32) -> Result<Vec<u8>, String> {
    let len = u64::from(h.num_entries) * u64::from(h.entry_size);
    if len > MAX_ENTRY_ARRAY_BYTES {
        return Err(format!(
            "partition array of {len} bytes is implausibly large"
        ));
    }
    let off = h
        .entries_lba
        .checked_mul(u64::from(ss))
        .ok_or("entry array LBA overflows")?;
    let data = src
        .read_vec_at(off, len as usize)
        .map_err(|e| e.to_string())?;
    if crc32fast::hash(&data) != h.entries_crc {
        return Err("partition array CRC mismatch".into());
    }
    Ok(data)
}

/// Parses a GPT assuming `ss`-byte sectors. Falls back to the backup header
/// and array when the primary copy is damaged.
pub(crate) fn parse(src: &dyn ReadAt, ss: u32) -> Result<Option<VolumeSystem>, VolumeError> {
    let disk_sectors = src.size() / u64::from(ss);
    if disk_sectors < 3 {
        return Ok(None);
    }
    let mut warnings = Vec::new();

    let primary = match read_header(src, 1, ss) {
        Ok(Some(h)) => match read_entries(src, &h, ss) {
            Ok(entries) => Some((h, entries)),
            Err(e) => {
                warnings.push(format!("primary GPT: {e}"));
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            warnings.push(format!("primary GPT: {e}"));
            None
        }
    };

    let (header, entries) = match primary {
        Some(p) => p,
        None => {
            // Try the last sector of the disk for the backup header.
            match read_header(src, disk_sectors - 1, ss) {
                Ok(Some(h)) => match read_entries(src, &h, ss) {
                    Ok(e) => {
                        warnings.push("using backup GPT".into());
                        (h, e)
                    }
                    Err(e) => {
                        return Err(corrupt(format!("primary and backup GPT unusable: {e}")));
                    }
                },
                Ok(None) if warnings.is_empty() => return Ok(None),
                Ok(None) => return Err(corrupt(warnings.join("; "))),
                Err(e) => return Err(corrupt(format!("{}; backup GPT: {e}", warnings.join("; ")))),
            }
        }
    };

    let mut slots = vec![
        meta(0, 1, "Safety Table".into()),
        meta(header.my_lba, 1, "GPT Header".into()),
    ];
    let entry_sectors =
        (u64::from(header.num_entries) * u64::from(header.entry_size)).div_ceil(u64::from(ss));
    slots.push(meta(
        header.entries_lba,
        entry_sectors,
        "Partition Table".into(),
    ));

    for (i, e) in entries.chunks_exact(header.entry_size as usize).enumerate() {
        let ty = Uuid::from_bytes_le(e.array(0).map_err(|x| corrupt(x.to_string()))?);
        if ty.is_nil() {
            continue;
        }
        let unique = Uuid::from_bytes_le(e.array(16).map_err(|x| corrupt(x.to_string()))?);
        let first = e.u64_le(32).map_err(|x| corrupt(x.to_string()))?;
        let last = e.u64_le(40).map_err(|x| corrupt(x.to_string()))?;
        let attrs = e.u64_le(48).map_err(|x| corrupt(x.to_string()))?;
        if last < first {
            warnings.push(format!(
                "entry {i}: last LBA {last} before first LBA {first}; skipped"
            ));
            continue;
        }
        if last >= disk_sectors {
            warnings.push(format!(
                "entry {i}: ends at LBA {last} beyond image end {disk_sectors}"
            ));
        }
        let name = utf16le_lossy(e.get(56..128).unwrap_or_default());
        slots.push(Partition {
            addr: 0,
            kind: SlotKind::Allocated,
            start_sector: first,
            sector_count: last - first + 1,
            ptype: Some(PartitionType::Gpt(ty)),
            // Autopsy describes GPT partitions by their name, falling back to type.
            description: if name.is_empty() {
                gpt_type_name(&ty).map_or_else(|| ty.hyphenated().to_string(), str::to_owned)
            } else {
                name.clone()
            },
            name: (!name.is_empty()).then_some(name),
            unique_id: Some(unique),
            table: Some(0),
            slot: Some(i as u32),
            flags: attrs,
        });
    }

    Ok(Some(VolumeSystem {
        kind: VsKind::Gpt,
        sector_size: ss,
        disk_sectors,
        disk_guid: Some(header.disk_guid),
        partitions: layout::finish(slots, disk_sectors),
        warnings,
    }))
}

fn meta(start: u64, count: u64, description: String) -> Partition {
    Partition {
        addr: 0,
        kind: SlotKind::Meta,
        start_sector: start,
        sector_count: count,
        ptype: None,
        description,
        name: None,
        unique_id: None,
        table: None,
        slot: None,
        flags: 0,
    }
}

fn corrupt(reason: String) -> VolumeError {
    VolumeError::Corrupt {
        kind: VsKind::Gpt,
        reason,
    }
}

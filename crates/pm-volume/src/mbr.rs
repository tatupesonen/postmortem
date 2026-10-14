//! DOS/MBR partition tables, including extended (EBR) chains.

use std::collections::HashSet;

use pm_core::{Bytes, ReadAt};

use crate::types::{is_extended, mbr_type_name};
use crate::{Partition, PartitionType, SlotKind, VolumeError, VolumeSystem, VsKind, layout};

/// Upper bound on extended tables followed (loop/abuse protection).
const MAX_EXTENDED_TABLES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) boot: u8,
    pub(crate) ptype: u8,
    pub(crate) start: u32,
    pub(crate) sectors: u32,
}

/// Reads the four entries of the table in `sector` (512-byte sectors).
///
/// Returns `None` when the sector lacks the `55 AA` signature or any boot
/// indicator is not `0x00`/`0x80` (typical of filesystem boot sectors, which
/// also end in `55 AA`).
pub(crate) fn read_table(src: &dyn ReadAt, sector: u64) -> Result<Option<[Entry; 4]>, VolumeError> {
    let mut buf = [0u8; 512];
    match src.read_exact_at(sector * 512, &mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    if buf.u16_le(510).ok() != Some(0xAA55) {
        return Ok(None);
    }
    let mut entries = [Entry {
        boot: 0,
        ptype: 0,
        start: 0,
        sectors: 0,
    }; 4];
    for (i, entry) in entries.iter_mut().enumerate() {
        let off = 446 + i * 16;
        let e = Entry {
            boot: buf.u8_at(off).unwrap_or(0),
            ptype: buf.u8_at(off + 4).unwrap_or(0),
            start: buf.u32_le(off + 8).unwrap_or(0),
            sectors: buf.u32_le(off + 12).unwrap_or(0),
        };
        if e.boot != 0 && e.boot != 0x80 {
            return Ok(None);
        }
        *entry = e;
    }
    Ok(Some(entries))
}

#[expect(
    clippy::too_many_arguments,
    reason = "private constructor mirroring Partition's fields"
)]
fn slot(
    kind: SlotKind,
    start: u64,
    count: u64,
    ptype: Option<u8>,
    description: String,
    table: u32,
    slot: Option<u32>,
    flags: u64,
) -> Partition {
    Partition {
        addr: 0,
        kind,
        start_sector: start,
        sector_count: count,
        ptype: ptype.map(PartitionType::Mbr),
        description,
        name: None,
        unique_id: None,
        table: Some(table),
        slot,
        flags,
    }
}

fn describe(ptype: u8) -> String {
    format!("{} (0x{ptype:02x})", mbr_type_name(ptype))
}

/// Parses a DOS volume system with the given sector size.
pub(crate) fn parse(
    src: &dyn ReadAt,
    sector_size: u32,
) -> Result<Option<VolumeSystem>, VolumeError> {
    let Some(primary) = read_table(src, 0)? else {
        return Ok(None);
    };
    let used: Vec<_> = primary
        .iter()
        .filter(|e| e.ptype != 0 && e.sectors != 0)
        .collect();
    if used.is_empty() {
        return Ok(None);
    }
    // A primary entry starting at sector 0 would overlap the MBR itself; this
    // pattern (with a 55AA signature) is a filesystem boot sector, not a table.
    if used.iter().any(|e| e.start == 0) {
        return Ok(None);
    }

    let disk_sectors = src.size() / u64::from(sector_size);
    let mut warnings = Vec::new();
    let mut slots = vec![slot(
        SlotKind::Meta,
        0,
        1,
        None,
        "Primary Table (#0)".to_owned(),
        0,
        None,
        0,
    )];
    let mut table_no = 0u32;

    for (i, e) in primary.iter().enumerate() {
        if e.ptype == 0 || e.sectors == 0 {
            continue;
        }
        let (start, count) = (u64::from(e.start), u64::from(e.sectors));
        if start + count > disk_sectors {
            warnings.push(format!(
                "partition in primary slot {i} ends at sector {} beyond image end {disk_sectors}",
                start + count
            ));
        }
        if is_extended(e.ptype) {
            slots.push(slot(
                SlotKind::Meta,
                start,
                count,
                Some(e.ptype),
                describe(e.ptype),
                0,
                Some(i as u32),
                0,
            ));
            walk_extended(src, start, &mut table_no, &mut slots, &mut warnings)?;
        } else {
            slots.push(slot(
                SlotKind::Allocated,
                start,
                count,
                Some(e.ptype),
                describe(e.ptype),
                0,
                Some(i as u32),
                u64::from(e.boot),
            ));
        }
    }

    Ok(Some(VolumeSystem {
        kind: VsKind::Dos,
        sector_size,
        disk_sectors,
        disk_guid: None,
        partitions: layout::finish(slots, disk_sectors),
        warnings,
    }))
}

/// Follows the EBR chain of an extended partition starting at `ext_base`.
///
/// Logical partition starts are relative to their EBR; links to the next EBR
/// are relative to the start of the outermost extended partition.
fn walk_extended(
    src: &dyn ReadAt,
    ext_base: u64,
    table_no: &mut u32,
    slots: &mut Vec<Partition>,
    warnings: &mut Vec<String>,
) -> Result<(), VolumeError> {
    let mut visited = HashSet::new();
    let mut ebr = ext_base;
    while visited.insert(ebr) {
        if visited.len() > MAX_EXTENDED_TABLES {
            warnings.push(format!(
                "extended partition chain exceeds {MAX_EXTENDED_TABLES} tables; truncated"
            ));
            break;
        }
        let Some(entries) = read_table(src, ebr)? else {
            warnings.push(format!("invalid extended partition table at sector {ebr}"));
            break;
        };
        *table_no += 1;
        let t = *table_no;
        slots.push(slot(
            SlotKind::Meta,
            ebr,
            1,
            None,
            format!("Extended Table (#{t})"),
            t,
            None,
            0,
        ));

        let mut next = None;
        for (i, e) in entries.iter().enumerate() {
            if e.ptype == 0 || e.sectors == 0 {
                continue;
            }
            if is_extended(e.ptype) {
                if next.is_none() {
                    next = Some(ext_base + u64::from(e.start));
                    slots.push(slot(
                        SlotKind::Meta,
                        ext_base + u64::from(e.start),
                        u64::from(e.sectors),
                        Some(e.ptype),
                        describe(e.ptype),
                        t,
                        Some(i as u32),
                        0,
                    ));
                }
            } else {
                slots.push(slot(
                    SlotKind::Allocated,
                    ebr + u64::from(e.start),
                    u64::from(e.sectors),
                    Some(e.ptype),
                    describe(e.ptype),
                    t,
                    Some(i as u32),
                    u64::from(e.boot),
                ));
            }
        }
        match next {
            Some(n) => ebr = n,
            None => return Ok(()),
        }
    }
    if visited.contains(&ebr) && visited.len() <= MAX_EXTENDED_TABLES {
        warnings.push(format!(
            "extended partition chain loops back to sector {ebr}"
        ));
    }
    Ok(())
}

//! Turns a list of table-derived slots into a gap-free disk layout.

use crate::{Partition, SlotKind};

/// Sorts `slots`, adds `Unallocated` gaps for sectors not covered by any
/// allocated partition, and assigns sequential addresses.
///
/// Matches Autopsy (`mmls`) exactly: only allocated partitions cover space, so
/// gaps may overlap `Meta` slots (the MBR sector, EBRs, GPT header) and
/// unused space inside an extended container is reported as unallocated.
pub(crate) fn finish(mut slots: Vec<Partition>, disk_sectors: u64) -> Vec<Partition> {
    let mut covered: Vec<(u64, u64)> = slots
        .iter()
        .filter(|p| p.kind == SlotKind::Allocated)
        .map(|p| {
            (
                p.start_sector,
                p.end_sector().min(disk_sectors.max(p.start_sector)),
            )
        })
        .filter(|(s, e)| e > s)
        .collect();
    covered.sort_unstable();

    let mut cursor = 0u64;
    let mut gaps = Vec::new();
    for (s, e) in covered {
        if s > cursor && cursor < disk_sectors {
            gaps.push((cursor, s.min(disk_sectors)));
        }
        cursor = cursor.max(e);
    }
    if cursor < disk_sectors {
        gaps.push((cursor, disk_sectors));
    }
    slots.extend(gaps.into_iter().map(|(s, e)| Partition {
        addr: 0,
        kind: SlotKind::Unallocated,
        start_sector: s,
        sector_count: e - s,
        ptype: None,
        description: "Unallocated".to_owned(),
        name: None,
        unique_id: None,
        table: None,
        slot: None,
        flags: 0,
    }));

    slots.sort_by_key(|p| {
        let rank = match p.kind {
            SlotKind::Meta if is_container(p) => 0,
            SlotKind::Meta => 1,
            SlotKind::Allocated => 2,
            SlotKind::Unallocated => 3,
        };
        (p.start_sector, rank, p.table, p.slot)
    });
    for (i, p) in slots.iter_mut().enumerate() {
        p.addr = u32::try_from(i).unwrap_or(u32::MAX);
    }
    slots
}

/// A meta slot that contains other slots (DOS extended partitions).
pub(crate) fn is_container(p: &Partition) -> bool {
    p.kind == SlotKind::Meta && p.sector_count > 1 && p.slot.is_some()
}

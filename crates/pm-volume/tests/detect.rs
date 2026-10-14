#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use pm_core::ReadAt;
use pm_core::io::MemSource;
use pm_testkit::{DiskBuilder, GptPart, MbrPart};
use pm_volume::{PartitionType, SlotKind, VsKind, detect};
use proptest::prelude::*;
use uuid::uuid;

const BASIC_DATA: uuid::Uuid = uuid!("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7");
const EFI: uuid::Uuid = uuid!("C12A7328-F81F-11D2-BA4B-00A0C93EC93B");

fn src(d: &DiskBuilder) -> MemSource {
    MemSource::new(d.build())
}

/// Compact (kind, start, count, description) view of a layout.
fn layout(vs: &pm_volume::VolumeSystem) -> Vec<(SlotKind, u64, u64, String)> {
    vs.partitions
        .iter()
        .map(|p| {
            (
                p.kind,
                p.start_sector,
                p.sector_count,
                p.description.clone(),
            )
        })
        .collect()
}

#[test]
fn blank_disk_has_no_volume_system() {
    let d = DiskBuilder::new(512, 64);
    assert!(detect(&src(&d)).unwrap().is_none());
}

#[test]
fn tiny_source_is_not_an_error() {
    assert!(detect(&MemSource::new(vec![0u8; 100])).unwrap().is_none());
}

#[test]
fn single_primary_partition_layout_matches_tsk() {
    use SlotKind::{Allocated, Meta, Unallocated};
    let mut d = DiskBuilder::new(512, 8192);
    d.mbr_table(
        0,
        &[MbrPart {
            boot: true,
            ..MbrPart::new(0x07, 2048, 4096)
        }],
    );
    let vs = detect(&src(&d)).unwrap().unwrap();
    assert_eq!(vs.kind, VsKind::Dos);
    assert_eq!(vs.sector_size, 512);
    assert_eq!(
        layout(&vs),
        vec![
            (Meta, 0, 1, "Primary Table (#0)".into()),
            (Unallocated, 1, 2047, "Unallocated".into()),
            (Allocated, 2048, 4096, "NTFS / exFAT (0x07)".into()),
            (Unallocated, 6144, 2048, "Unallocated".into()),
        ]
    );
    let addrs: Vec<u32> = vs.partitions.iter().map(|p| p.addr).collect();
    assert_eq!(addrs, vec![0, 1, 2, 3]);
    let p = vs.allocated().next().unwrap();
    assert_eq!(p.flags, 0x80, "boot flag preserved");
    assert_eq!(p.ptype, Some(PartitionType::Mbr(0x07)));
    assert!(vs.warnings.is_empty());
}

#[test]
fn extended_chain_resolves_relative_offsets() {
    // Extended partition at 4000..8000 holding two logical partitions.
    let mut d = DiskBuilder::new(512, 10_000);
    d.mbr_table(
        0,
        &[MbrPart::new(0x0B, 63, 3937), MbrPart::new(0x0F, 4000, 4000)],
    );
    // EBR #1 at 4000: logical at +63 (abs 4063), next EBR at ext+2000.
    d.mbr_table(
        4000,
        &[MbrPart::new(0x83, 63, 1900), MbrPart::new(0x05, 2000, 2000)],
    );
    // EBR #2 at 6000: logical at +63 (abs 6063); unused tail to 8000.
    d.mbr_table(6000, &[MbrPart::new(0x82, 63, 1000)]);

    let vs = detect(&src(&d)).unwrap().unwrap();
    let alloc: Vec<_> = vs
        .allocated()
        .map(|p| (p.start_sector, p.sector_count, p.table))
        .collect();
    assert_eq!(
        alloc,
        vec![
            (63, 3937, Some(0)),
            (4063, 1900, Some(1)),
            (6063, 1000, Some(2))
        ]
    );

    let metas: Vec<_> = vs
        .partitions
        .iter()
        .filter(|p| p.kind == SlotKind::Meta)
        .map(|p| (p.start_sector, p.description.as_str()))
        .collect();
    assert_eq!(
        metas,
        vec![
            (0, "Primary Table (#0)"),
            (4000, "Win95 Extended (0x0f)"),
            (4000, "Extended Table (#1)"),
            (6000, "DOS Extended (0x05)"),
            (6000, "Extended Table (#2)"),
        ]
    );
    // Space inside the extended container not used by logicals is unallocated.
    let unalloc: Vec<_> = vs
        .partitions
        .iter()
        .filter(|p| p.kind == SlotKind::Unallocated)
        .map(|p| (p.start_sector, p.sector_count))
        .collect();
    assert_eq!(
        unalloc,
        vec![(1, 62), (4001, 62), (5963, 37), (6001, 62), (7063, 2937)]
    );
}

#[test]
fn looping_extended_chain_terminates_with_warning() {
    let mut d = DiskBuilder::new(512, 4096);
    d.mbr_table(0, &[MbrPart::new(0x05, 1000, 2000)]);
    // EBR links back to itself (relative 0 → ext base).
    d.mbr_table(
        1000,
        &[MbrPart::new(0x83, 10, 100), MbrPart::new(0x05, 0, 2000)],
    );
    let vs = detect(&src(&d)).unwrap().unwrap();
    assert_eq!(vs.allocated().count(), 1);
    assert!(
        vs.warnings.iter().any(|w| w.contains("loops")),
        "{:?}",
        vs.warnings
    );
}

#[test]
fn filesystem_boot_sector_is_not_a_partition_table() {
    let mut d = DiskBuilder::new(512, 64);
    // FAT boot sector: jump + OEM; partition-entry area holds boot code bytes.
    d.write(
        0,
        &[
            0xEB, 0x3C, 0x90, b'M', b'S', b'D', b'O', b'S', b'5', b'.', b'0',
        ],
    );
    d.write(446, &[0x33, 0xC9, 0x8E, 0xD1, 0xBC, 0xF0, 0x7B, 0x8E]);
    d.write(510, &[0x55, 0xAA]);
    assert!(detect(&src(&d)).unwrap().is_none());
}

#[test]
fn partition_past_end_of_image_warns() {
    let mut d = DiskBuilder::new(512, 1000);
    d.mbr_table(0, &[MbrPart::new(0x83, 100, 5000)]);
    let vs = detect(&src(&d)).unwrap().unwrap();
    assert_eq!(vs.warnings.len(), 1);
    assert!(vs.warnings[0].contains("beyond image end"));
    // No unallocated slot is invented past the image.
    assert!(
        vs.partitions
            .iter()
            .all(|p| p.kind != SlotKind::Unallocated || p.end_sector() <= 1000)
    );
}

#[test]
fn open_partition_reads_its_bytes() {
    let mut d = DiskBuilder::new(512, 4096);
    d.mbr_table(0, &[MbrPart::new(0x83, 2048, 16)]);
    d.fill_sectors(2048, 16, 0xAB);
    let disk = src(&d);
    let vs = detect(&disk).unwrap().unwrap();
    let p = vs.allocated().next().unwrap();
    let part = vs.open(disk.clone(), p);
    assert_eq!(part.size(), 16 * 512);
    assert!(
        part.read_vec_at(0, 8192)
            .unwrap()
            .iter()
            .all(|&b| b == 0xAB)
    );
    assert!(part.read_vec_at(8192, 1).is_err());
}

fn gpt_disk(ss: usize, sectors: u64) -> DiskBuilder {
    let mut d = DiskBuilder::new(ss, sectors);
    d.gpt(&[
        GptPart::new(EFI, 40, 239, "EFI system partition"),
        GptPart::new(BASIC_DATA, 240, sectors - 100, "Basic data partition"),
    ]);
    d
}

#[test]
fn gpt_partitions_names_and_meta() {
    let d = gpt_disk(512, 4096);
    let vs = detect(&src(&d)).unwrap().unwrap();
    assert_eq!(vs.kind, VsKind::Gpt);
    assert!(vs.warnings.is_empty(), "{:?}", vs.warnings);
    assert_eq!(
        vs.disk_guid.unwrap(),
        uuid::Uuid::from_u128(0xD15C_0000_0000_0000_0000_0000_0000_0001)
    );
    let alloc: Vec<_> = vs
        .allocated()
        .map(|p| {
            (
                p.start_sector,
                p.sector_count,
                p.description.as_str(),
                p.name.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        alloc,
        vec![
            (
                40,
                200,
                "EFI System Partition",
                Some("EFI system partition")
            ),
            (
                240,
                3757,
                "Basic Data Partition",
                Some("Basic data partition")
            ),
        ]
    );
    let metas: Vec<_> = vs
        .partitions
        .iter()
        .filter(|p| p.kind == SlotKind::Meta)
        .map(|p| p.description.as_str())
        .collect();
    assert_eq!(
        metas,
        vec![
            "Safety Table",
            "GPT Header",
            "Partition Table",
            "Backup GPT Header"
        ]
    );
}

#[test]
fn gpt_with_4k_sectors() {
    let d = gpt_disk(4096, 1024);
    let vs = detect(&src(&d)).unwrap().unwrap();
    assert_eq!(vs.kind, VsKind::Gpt);
    assert_eq!(vs.sector_size, 4096);
    let p = vs.allocated().nth(1).unwrap();
    assert_eq!(vs.byte_offset(p), 240 * 4096);
}

#[test]
fn gpt_falls_back_to_backup_header() {
    let mut d = gpt_disk(512, 4096);
    d.write(512 + 30, &[0xFF]); // corrupt primary header → CRC mismatch
    let vs = detect(&src(&d)).unwrap().unwrap();
    assert_eq!(vs.kind, VsKind::Gpt);
    assert_eq!(vs.allocated().count(), 2);
    assert!(
        vs.warnings.iter().any(|w| w.contains("CRC")),
        "{:?}",
        vs.warnings
    );
    assert!(vs.warnings.iter().any(|w| w.contains("backup")));
}

#[test]
fn gpt_corrupt_entries_use_backup_array() {
    let mut d = gpt_disk(512, 4096);
    d.write(2 * 512 + 60, &[0x41]); // damage primary partition array
    let vs = detect(&src(&d)).unwrap().unwrap();
    assert_eq!(vs.allocated().count(), 2);
    assert!(vs.warnings.iter().any(|w| w.contains("array CRC")));
}

#[test]
fn protective_mbr_without_gpt_falls_back_to_dos() {
    let mut d = DiskBuilder::new(512, 2048);
    d.mbr_table(0, &[MbrPart::new(0xEE, 1, 2047)]);
    let vs = detect(&src(&d)).unwrap().unwrap();
    assert_eq!(vs.kind, VsKind::Dos);
    assert_eq!(
        vs.allocated().next().unwrap().description,
        "GPT Safety Partition (0xee)"
    );
}

fn check_layout_invariants(vs: &pm_volume::VolumeSystem) {
    // Sorted by start, addresses sequential.
    for w in vs.partitions.windows(2) {
        assert!(w[0].start_sector <= w[1].start_sector);
        assert_eq!(w[0].addr + 1, w[1].addr);
    }
    // Unallocated slots lie within the disk and never overlap allocated ones.
    for u in vs
        .partitions
        .iter()
        .filter(|p| p.kind == SlotKind::Unallocated)
    {
        assert!(u.sector_count > 0 && u.end_sector() <= vs.disk_sectors);
        for a in vs.allocated() {
            assert!(u.end_sector() <= a.start_sector || u.start_sector >= a.end_sector());
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn random_sector_zero_never_panics(sector in proptest::collection::vec(any::<u8>(), 512), tail in 0usize..8) {
        let mut data = sector;
        data[510] = 0x55;
        data[511] = 0xAA;
        data.extend(std::iter::repeat_n(0u8, tail * 512));
        if let Ok(Some(vs)) = detect(&MemSource::new(data)) {
            check_layout_invariants(&vs);
        }
    }

    #[test]
    fn random_mbr_entries_produce_consistent_layouts(
        entries in proptest::collection::vec((prop_oneof![Just(0x05u8), Just(0x83u8), Just(0x07u8), any::<u8>()], 0u32..3000, 0u32..3000), 1..4),
        ebr in proptest::collection::vec((any::<u8>(), 0u32..3000, 0u32..3000), 0..4),
    ) {
        let mut d = DiskBuilder::new(512, 2048);
        let parts: Vec<_> = entries.iter().map(|&(t, s, n)| MbrPart::new(t, s, n)).collect();
        d.mbr_table(0, &parts);
        for &(_, s, _) in &entries {
            if u64::from(s) < 2048 {
                let e: Vec<_> = ebr.iter().map(|&(t, s, n)| MbrPart::new(t, s, n)).collect();
                d.mbr_table(u64::from(s), &e);
            }
        }
        if let Ok(Some(vs)) = detect(&src(&d)) {
            check_layout_invariants(&vs);
        }
    }

    #[test]
    fn gpt_bitflips_never_panic(pos in 0usize..(34 * 512), bit in 0u8..8) {
        let d = gpt_disk(512, 256);
        let mut data = d.build();
        data[pos] ^= 1 << bit;
        if let Ok(Some(vs)) = detect(&MemSource::new(data)) {
            check_layout_invariants(&vs);
        }
    }
}

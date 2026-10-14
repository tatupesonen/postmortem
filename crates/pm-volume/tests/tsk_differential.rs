//! Differential tests: postmortem's volume layout must match TSK's `mmls`.
//! Skipped (pass with a note) when TSK is not installed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use pm_core::io::MemSource;
use pm_testkit::tsk::{MmlsRow, mmls};
use pm_testkit::{DiskBuilder, GptPart, MbrPart};
use pm_volume::{SlotKind, detect};
use uuid::uuid;

fn ours(img: &[u8]) -> Vec<MmlsRow> {
    let vs = detect(&MemSource::new(img.to_vec())).unwrap().unwrap();
    vs.partitions
        .iter()
        .map(|p| MmlsRow {
            slot: match p.kind {
                SlotKind::Meta => "Meta".into(),
                SlotKind::Unallocated => "-------".into(),
                SlotKind::Allocated => match vs.kind {
                    pm_volume::VsKind::Dos => {
                        format!("{:03}:{:03}", p.table.unwrap(), p.slot.unwrap())
                    }
                    pm_volume::VsKind::Gpt => format!("{:03}", p.slot.unwrap()),
                },
            },
            start: p.start_sector,
            len: p.sector_count,
            description: p.description.clone(),
        })
        .collect()
}

fn compare(name: &str, img: &[u8]) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{name}.dd"));
    std::fs::write(&path, img).unwrap();
    let Some(theirs) = mmls(&path) else {
        eprintln!("mmls not available; skipping {name}");
        return;
    };
    assert_eq!(ours(img), theirs, "layout differs from mmls for {name}");
}

#[test]
fn dos_with_extended_chain() {
    let mut d = DiskBuilder::new(512, 10_000);
    d.mbr_table(
        0,
        &[MbrPart::new(0x0B, 63, 3937), MbrPart::new(0x0F, 4000, 4000)],
    );
    d.mbr_table(
        4000,
        &[MbrPart::new(0x83, 63, 1900), MbrPart::new(0x05, 2000, 2000)],
    );
    d.mbr_table(6000, &[MbrPart::new(0x82, 63, 1000)]);
    compare("ext", &d.build());
}

#[test]
fn dos_primary_partitions_with_gaps() {
    let mut d = DiskBuilder::new(512, 20_000);
    d.mbr_table(
        0,
        &[
            MbrPart {
                boot: true,
                ..MbrPart::new(0x07, 2048, 4096)
            },
            MbrPart::new(0x83, 8192, 2048),
            MbrPart::new(0x8E, 12_000, 1000),
        ],
    );
    compare("primary", &d.build());
}

#[test]
fn gpt_named_partitions() {
    let mut g = DiskBuilder::new(512, 4096);
    g.gpt(&[
        GptPart::new(
            uuid!("C12A7328-F81F-11D2-BA4B-00A0C93EC93B"),
            40,
            239,
            "EFI",
        ),
        GptPart::new(
            uuid!("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"),
            240,
            3996,
            "Data",
        ),
    ]);
    compare("gpt", &g.build());
}

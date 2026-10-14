#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use pm_evidence::{OpenOptions, open};
use pm_fs::FsKind;
use pm_testkit::ntfs::NtfsBuilder;
use pm_testkit::{DiskBuilder, MbrPart};

#[test]
fn partitioned_disk_with_ntfs_and_fat() {
    let ntfs = NtfsBuilder::new(4).build();
    let mut fat = std::io::Cursor::new(vec![0u8; 2 * 1024 * 1024]);
    fatfs::format_volume(
        &mut fat,
        fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat12),
    )
    .unwrap();
    let fat = fat.into_inner();

    let ntfs_start = 2048u64;
    let ntfs_sectors = (ntfs.len() / 512) as u64;
    let fat_start = ntfs_start + ntfs_sectors + 2048;
    let fat_sectors = (fat.len() / 512) as u64;
    let mut d = DiskBuilder::new(512, fat_start + fat_sectors + 100);
    d.mbr_table(
        0,
        &[
            MbrPart::new(0x07, ntfs_start as u32, ntfs_sectors as u32),
            MbrPart::new(0x01, fat_start as u32, fat_sectors as u32),
        ],
    );
    d.write(ntfs_start as usize * 512, &ntfs);
    d.write(fat_start as usize * 512, &fat);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.dd");
    std::fs::write(&path, d.build()).unwrap();
    let ev = open(&path, &OpenOptions::default()).unwrap();
    assert!(ev.volume_system.is_some());
    let kinds: Vec<FsKind> = ev.filesystems.iter().map(|f| f.fs.info().kind).collect();
    assert_eq!(kinds, [FsKind::Ntfs, FsKind::Fat12]);
    assert_eq!(ev.filesystems[0].offset, ntfs_start * 512);
    let p = ev.filesystems[1].partition.as_ref().unwrap();
    assert!(ev.fs_for_partition(p.addr).is_some());
}

#[test]
fn bare_volume_without_partition_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vol.dd");
    std::fs::write(&path, NtfsBuilder::new(4).build()).unwrap();
    let ev = open(&path, &OpenOptions::default()).unwrap();
    assert!(ev.volume_system.is_none());
    assert_eq!(ev.filesystems.len(), 1);
    assert!(ev.filesystems[0].partition.is_none());
}

#[test]
fn unknown_content_yields_no_filesystems() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zeros.dd");
    std::fs::write(&path, vec![0u8; 1 << 20]).unwrap();
    let ev = open(&path, &OpenOptions::default()).unwrap();
    assert!(ev.volume_system.is_none() && ev.filesystems.is_empty());
}

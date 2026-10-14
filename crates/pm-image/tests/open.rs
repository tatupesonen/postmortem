#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
use std::fs;

use pm_core::io::MemSource;
use pm_image::{ImageError, ImageFormat, open, sniff};
use rstest::rstest;

#[test]
fn opens_single_raw_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("disk.dd");
    fs::write(&p, vec![7u8; 4096]).unwrap();
    let img = open(&p).unwrap();
    assert_eq!(img.info.format, ImageFormat::Raw);
    assert_eq!(img.info.size, 4096);
    assert_eq!(img.info.paths, vec![p]);
    assert_eq!(img.source().read_vec_at(4000, 96).unwrap(), vec![7u8; 96]);
}

#[test]
fn opens_split_raw_set_as_one_disk() {
    let dir = tempfile::tempdir().unwrap();
    for (i, byte) in [(1, b'a'), (2, b'b'), (3, b'c')] {
        fs::write(dir.path().join(format!("img.{i:03}")), vec![byte; 1000]).unwrap();
    }
    let img = open(dir.path().join("img.001")).unwrap();
    assert_eq!(img.info.paths.len(), 3);
    assert_eq!(img.info.size, 3000);
    let mid = img.source().read_vec_at(990, 20).unwrap();
    assert_eq!(&mid[..10], &[b'a'; 10]);
    assert_eq!(&mid[10..], &[b'b'; 10]);
}

#[test]
fn missing_file_reports_path() {
    let err = open("/nonexistent/postmortem/disk.dd").unwrap_err();
    assert!(matches!(err, ImageError::Open { .. }));
    assert!(err.to_string().contains("disk.dd"));
}

#[rstest]
#[case(b"EVF\x09\x0d\x0a\xff\x00rest", ImageFormat::Ewf)]
#[case(b"EVF2\x0d\x0a\x81\x00rest", ImageFormat::Ewf2)]
#[case(b"KDMV\x01\x00\x00\x00", ImageFormat::Vmdk)]
#[case(b"# Disk DescriptorFile\n", ImageFormat::Vmdk)]
#[case(b"vhdxfile", ImageFormat::Vhdx)]
#[case(b"QFI\xfb\x00\x00\x00\x03", ImageFormat::Qcow)]
#[case(b"conectix", ImageFormat::Vhd)]
#[case(b"\x00\x00\x00\x00", ImageFormat::Raw)]
fn sniffs_leading_magic(#[case] head: &[u8], #[case] want: ImageFormat) {
    let mut data = head.to_vec();
    data.resize(2048, 0);
    assert_eq!(sniff(&MemSource::new(data)), want);
}

#[test]
fn sniffs_trailing_footers() {
    let mut vhd = vec![0u8; 4096];
    vhd[4096 - 512..4096 - 504].copy_from_slice(b"conectix");
    assert_eq!(sniff(&MemSource::new(vhd)), ImageFormat::Vhd);
    let mut dmg = vec![0u8; 4096];
    dmg[4096 - 512..4096 - 508].copy_from_slice(b"koly");
    assert_eq!(sniff(&MemSource::new(dmg)), ImageFormat::Dmg);
    assert_eq!(sniff(&MemSource::new(vec![])), ImageFormat::Raw);
}

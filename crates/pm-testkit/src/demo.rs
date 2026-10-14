//! A small but realistic demo disk used for manual UI testing, screenshots
//! and end-to-end UI tests: MBR with an NTFS system partition and a FAT32
//! "USB" partition, populated with documents, pictures, deleted files, an
//! alternate data stream and a compressed file.

use std::io::{Cursor, Write};

use crate::ntfs::{Deleted, FileOpts, NtfsBuilder};
use crate::{DiskBuilder, MbrPart};

/// An uncompressed 24-bit BMP with a colour gradient.
pub fn gradient_bmp(w: u32, h: u32) -> Vec<u8> {
    let row = (w * 3).next_multiple_of(4);
    let data_len = row * h;
    let mut v = Vec::with_capacity(54 + data_len as usize);
    v.extend_from_slice(b"BM");
    v.extend_from_slice(&(54 + data_len).to_le_bytes());
    v.extend_from_slice(&[0; 4]);
    v.extend_from_slice(&54u32.to_le_bytes());
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&24u16.to_le_bytes());
    v.extend_from_slice(&[0; 24]);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / w.max(1)) as u8;
            let g = (y * 255 / h.max(1)) as u8;
            let b = (((x + y) * 3) % 256) as u8;
            v.extend_from_slice(&[b, g, r]);
        }
        v.extend(std::iter::repeat_n(0u8, (row - w * 3) as usize));
    }
    v
}

fn ntfs_volume() -> Vec<u8> {
    let mut b = NtfsBuilder::new(24).label("WINDOWS");
    b.dir("Users");
    b.dir("Users/alice");
    b.dir("Users/alice/Documents");
    b.dir("Users/alice/Pictures");
    b.dir("Users/alice/Downloads");
    b.file("Users/alice/Documents/meeting notes.txt", "Meeting 2021-03-04\r\n- Q1 budget approved\r\n- Server migration on Friday\r\n- Contact: bob@example.com, +358 40 123 4567\r\n".as_bytes());
    b.file(
        "Users/alice/Documents/passwords.txt",
        b"router: hunter2\nwifi: correct-horse-battery-staple\n",
    );
    b.file_with(
        "Users/alice/Documents/project_plan.txt",
        &"Phase 1: requirements gathering and stakeholder interviews.\n"
            .repeat(400)
            .into_bytes(),
        FileOpts {
            fragmented: true,
            ..Default::default()
        },
    );
    b.file("Users/alice/Pictures/sunset.bmp", &gradient_bmp(320, 200));
    b.file("Users/alice/Pictures/thumb.bmp", &gradient_bmp(64, 48));
    b.file_with(
        "Users/alice/Downloads/setup.exe",
        &[b"MZ\x90\x00".as_slice(), &[0u8; 60], b"This program cannot be run in DOS mode.\r\r\n$", &vec![0xCCu8; 8000]].concat(),
        FileOpts { ads: vec![("Zone.Identifier".into(), b"[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/\r\nHostUrl=https://example.com/setup.exe\r\n".to_vec())], ..Default::default() },
    );
    b.file_with(
        "Users/alice/Downloads/secret_plan.docx",
        b"PK\x03\x04 this would be a docx; deleted but recoverable",
        FileOpts {
            deleted: Deleted::InSlack,
            ..Default::default()
        },
    );
    b.file_with(
        "Users/alice/Documents/old_notes.txt",
        b"Deleted notes: meet at the pier at 23:00.\n",
        FileOpts {
            deleted: Deleted::Unindexed,
            ..Default::default()
        },
    );
    b.file_with(
        "evidence_shredded.log",
        b"orphaned file content\n",
        FileOpts {
            deleted: Deleted::Orphan,
            ..Default::default()
        },
    );
    b.dir("Windows");
    b.dir("Windows/Logs");
    b.file_with(
        "Windows/Logs/CBS.log",
        &"2021-03-04 10:20:30, Info  CBS  Session initialised by client\n"
            .repeat(2000)
            .into_bytes(),
        FileOpts {
            compressed: true,
            ..Default::default()
        },
    );
    b.build()
}

fn fat_volume() -> Vec<u8> {
    let mut cur = Cursor::new(vec![0u8; 40 * 1024 * 1024]);
    fatfs::format_volume(
        &mut cur,
        fatfs::FormatVolumeOptions::new()
            .fat_type(fatfs::FatType::Fat32)
            .volume_label(*b"USB STICK  "),
    )
    .unwrap();
    {
        let fs = fatfs::FileSystem::new(&mut cur, fatfs::FsOptions::new()).unwrap();
        let root = fs.root_dir();
        root.create_dir("DCIM").unwrap();
        root.create_file("DCIM/IMG_0001.bmp")
            .unwrap()
            .write_all(&gradient_bmp(200, 150))
            .unwrap();
        root.create_file("readme.txt")
            .unwrap()
            .write_all(b"USB stick belonging to alice\n")
            .unwrap();
        root.create_file("Tax return 2020 (final).txt")
            .unwrap()
            .write_all(b"Income: 42 000 EUR\n")
            .unwrap();
        root.create_file("deleted_photo.bmp")
            .unwrap()
            .write_all(&gradient_bmp(80, 60))
            .unwrap();
        root.remove("deleted_photo.bmp").unwrap();
        drop(root);
        fs.unmount().unwrap();
    }
    cur.into_inner()
}

/// Builds the demo disk image (raw).
pub fn demo_disk() -> Vec<u8> {
    let ntfs = ntfs_volume();
    let fat = fat_volume();
    let ntfs_start = 2048u64;
    let ntfs_sectors = (ntfs.len() / 512) as u64;
    let fat_start = (ntfs_start + ntfs_sectors).next_multiple_of(2048);
    let fat_sectors = (fat.len() / 512) as u64;
    let total = fat_start + fat_sectors + 2048;
    let mut d = DiskBuilder::new(512, total);
    d.mbr_table(
        0,
        &[
            MbrPart {
                boot: true,
                ..MbrPart::new(0x07, ntfs_start as u32, ntfs_sectors as u32)
            },
            MbrPart::new(0x0C, fat_start as u32, fat_sectors as u32),
        ],
    );
    d.write(ntfs_start as usize * 512, &ntfs);
    d.write(fat_start as usize * 512, &fat);
    d.build()
}

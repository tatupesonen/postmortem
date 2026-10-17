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

/// FILETIME for a UTC wall-clock time.
fn ft(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
    // Days from civil (Howard Hinnant's algorithm).
    let (y, mo) = if mo <= 2 {
        (y - 1, mo + 9)
    } else {
        (y, mo - 3)
    };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u32;
    let doy = (153 * mo + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = i64::from(era) * 146_097 + i64::from(doe) - 719_468;
    let secs = days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60;
    ((secs + 11_644_473_600) * 10_000_000) as u64
}

/// [created, modified, MFT-changed, accessed] as `FileOpts::times`.
fn times(created: u64, modified: u64, accessed: u64) -> Option<[u64; 4]> {
    Some([created, modified, modified, accessed])
}

fn opts(t: Option<[u64; 4]>) -> FileOpts {
    FileOpts {
        times: t,
        ..Default::default()
    }
}

fn ntfs_volume() -> Vec<u8> {
    let mut b = NtfsBuilder::new(24).label("WINDOWS");
    b.dir("Users");
    b.dir("Users/alice");
    b.dir("Users/alice/Documents");
    b.dir("Users/alice/Pictures");
    b.dir("Users/alice/Downloads");
    b.file_with(
        "Users/alice/Documents/meeting notes.txt",
        "Meeting 2021-03-04\r\n- Q1 budget approved\r\n- Server migration on Friday\r\n- Contact: bob@example.com, +358 40 123 4567\r\n".as_bytes(),
        opts(times(ft(2021, 3, 4, 9, 2), ft(2021, 3, 4, 11, 47), ft(2021, 6, 1, 8, 15))),
    );
    b.file_with(
        "Users/alice/Documents/passwords.txt",
        b"router: hunter2\nwifi: correct-horse-battery-staple\n",
        opts(times(
            ft(2020, 11, 20, 22, 10),
            ft(2021, 5, 30, 23, 41),
            ft(2021, 6, 1, 7, 59),
        )),
    );
    b.file_with(
        "Users/alice/Documents/project_plan.txt",
        &"Phase 1: requirements gathering and stakeholder interviews.\n"
            .repeat(400)
            .into_bytes(),
        FileOpts {
            fragmented: true,
            times: times(
                ft(2021, 1, 11, 10, 0),
                ft(2021, 4, 28, 16, 20),
                ft(2021, 5, 2, 9, 30),
            ),
            ..Default::default()
        },
    );
    b.file_with(
        "Users/alice/Pictures/sunset.bmp",
        &gradient_bmp(320, 200),
        opts(times(
            ft(2020, 7, 14, 20, 33),
            ft(2020, 7, 14, 20, 33),
            ft(2021, 1, 2, 12, 0),
        )),
    );
    b.file_with(
        "Users/alice/Pictures/thumb.bmp",
        &gradient_bmp(64, 48),
        opts(times(
            ft(2020, 7, 14, 20, 34),
            ft(2020, 7, 14, 20, 34),
            ft(2020, 7, 14, 20, 34),
        )),
    );
    b.file_with(
        "Users/alice/Downloads/setup.exe",
        &[b"MZ\x90\x00".as_slice(), &[0u8; 60], b"This program cannot be run in DOS mode.\r\r\n$", &vec![0xCCu8; 8000]].concat(),
        FileOpts {
            times: times(ft(2021, 5, 31, 23, 58), ft(2021, 5, 31, 23, 58), ft(2021, 5, 31, 23, 59)),
            ads: vec![("Zone.Identifier".into(), b"[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/\r\nHostUrl=https://example.com/setup.exe\r\n".to_vec())],
            ..Default::default()
        },
    );
    // An archive: the Embedded File Extractor turns its members into
    // derived files (a picture and a text file in a sub-folder).
    b.file_with(
        "Users/alice/Downloads/holiday.zip",
        &crate::archives::zip(&[
            ("beach.bmp", &gradient_bmp(48, 32)),
            ("notes/readme.txt", b"Wish you were here.\n"),
        ]),
        opts(times(
            ft(2021, 5, 30, 18, 0),
            ft(2021, 5, 30, 18, 0),
            ft(2021, 5, 31, 9, 0),
        )),
    );
    b.file_with(
        "Users/alice/Downloads/secret_plan.docx",
        b"PK\x03\x04 this would be a docx; deleted but recoverable",
        FileOpts {
            deleted: Deleted::InSlack,
            times: times(
                ft(2021, 5, 15, 14, 0),
                ft(2021, 5, 31, 23, 50),
                ft(2021, 5, 31, 23, 55),
            ),
            ..Default::default()
        },
    );
    b.file_with(
        "Users/alice/Documents/old_notes.txt",
        b"Deleted notes: meet at the pier at 23:00.\n",
        FileOpts {
            deleted: Deleted::Unindexed,
            times: times(
                ft(2021, 5, 20, 21, 5),
                ft(2021, 5, 31, 23, 30),
                ft(2021, 5, 31, 23, 52),
            ),
            ..Default::default()
        },
    );
    b.file_with(
        "evidence_shredded.log",
        b"orphaned file content\n",
        FileOpts {
            deleted: Deleted::Orphan,
            times: times(
                ft(2021, 5, 31, 23, 45),
                ft(2021, 6, 1, 0, 3),
                ft(2021, 6, 1, 0, 4),
            ),
            ..Default::default()
        },
    );
    b.dir("Users/alice/Documents/Projects");
    for i in 0..36u32 {
        let (mo, d) = (1 + i % 12, 1 + (i * 7) % 27);
        let year = 2020 + (i / 12) as i32 % 2;
        let c = ft(year, mo, d, 9 + i % 8, (i * 13) % 60);
        let m = ft(year, mo, (d + 1).min(28), 10 + i % 8, (i * 17) % 60);
        b.file_with(
            &format!("Users/alice/Documents/Projects/report_{year}_{mo:02}_{i:02}.txt"),
            format!("Status report #{i}\nAll milestones on track.\n").as_bytes(),
            opts(times(c, m, m)),
        );
    }
    b.dir("Windows");
    b.dir("Windows/Logs");
    b.file_with(
        "Windows/Logs/CBS.log",
        &"2021-03-04 10:20:30, Info  CBS  Session initialised by client\n"
            .repeat(2000)
            .into_bytes(),
        FileOpts {
            compressed: true,
            times: times(
                ft(2020, 2, 1, 6, 0),
                ft(2021, 6, 1, 0, 10),
                ft(2021, 6, 1, 0, 10),
            ),
            ..Default::default()
        },
    );
    b.build()
}

/// fatfs clock for the USB volume: 2022-05-10 18:30:00.
#[derive(Debug)]
struct UsbClock;

impl fatfs::TimeProvider for UsbClock {
    fn get_current_date(&self) -> fatfs::Date {
        fatfs::Date {
            year: 2022,
            month: 5,
            day: 10,
        }
    }
    fn get_current_date_time(&self) -> fatfs::DateTime {
        fatfs::DateTime {
            date: self.get_current_date(),
            time: fatfs::Time {
                hour: 18,
                min: 30,
                sec: 0,
                millis: 0,
            },
        }
    }
}

static USB_CLOCK: UsbClock = UsbClock;

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
        let fs =
            fatfs::FileSystem::new(&mut cur, fatfs::FsOptions::new().time_provider(&USB_CLOCK))
                .unwrap();
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

/// [`demo_disk`] as a file shared by all test processes (see
/// [`crate::shared_file`]); read it, never write it.
pub fn demo_disk_file() -> &'static std::path::Path {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| crate::shared_file("demo", "dd", &demo_disk()))
}

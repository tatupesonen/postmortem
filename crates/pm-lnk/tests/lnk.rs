#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use pm_lnk::builder::{
    DestEntry, FileItem, LnkBuilder, compound_file, custom_destinations, dest_list, dos, file_item,
    id_list, my_computer, network_item, volume_item,
};
use pm_lnk::jumplist::{app_id, app_name, parse_automatic, parse_custom};
use pm_lnk::shell_item::{ItemKind, join_path, parse_id_list};
use pm_lnk::{Error, ShellLink};
use proptest::prelude::*;

/// 2004-08-20 15:00:00 UTC as FILETIME.
const FT: u64 = 127_374_876_000_000_000;
const FT_UNIX: i64 = 1_093_014_000;

fn file(short: &str, long: &str, is_dir: bool, version: u16) -> Vec<u8> {
    file_item(&FileItem {
        short_name: short,
        long_name: long,
        is_dir,
        size: 1234,
        modified: dos(2004, 8, 20, 15, 0, 0),
        created: dos(2004, 8, 19, 10, 30, 2),
        accessed: dos(2004, 8, 21, 0, 0, 0),
        version,
        mft: (0x1234, 3),
    })
}

fn sample_ids() -> Vec<u8> {
    id_list(&[
        my_computer(),
        volume_item("C:\\"),
        file("DOCUME~1", "Documents and Settings", true, 3),
        file("MR4E39~1.EVI", "Mr. Evil", true, 8),
        file("HACKIN~1.TXT", "hacking notes.txt", false, 9),
    ])
}

#[test]
fn shell_items_decode_names_times_and_references() {
    let items = parse_id_list(&sample_ids());
    assert_eq!(items.len(), 5);
    assert_eq!(items[0].kind, Some(ItemKind::Root));
    assert_eq!(items[0].name, "My Computer");
    assert_eq!(items[1].name, "C:\\");
    assert_eq!(items[2].name, "Documents and Settings");
    assert_eq!(items[2].short_name.as_deref(), Some("DOCUME~1"));
    assert!(items[2].is_directory);
    assert_eq!(items[3].name, "Mr. Evil");
    assert_eq!(items[3].mft_entry, Some(0x1234));
    assert_eq!(items[3].mft_sequence, Some(3));
    assert_eq!(items[2].mft_entry, None, "version 3 has no file reference");
    let f = &items[4];
    assert_eq!(f.name, "hacking notes.txt");
    assert!(!f.is_directory);
    assert_eq!(f.file_size, Some(1234));
    assert_eq!(f.modified, Some(FT_UNIX));
    assert_eq!(f.created, Some(1_092_911_402));
    assert_eq!(f.accessed, Some(1_093_046_400));
    assert_eq!(
        join_path(&items),
        "My Computer\\C:\\Documents and Settings\\Mr. Evil\\hacking notes.txt"
    );
    let net = parse_id_list(&id_list(&[network_item("\\\\SERVER\\share")]));
    assert_eq!(net[0].kind, Some(ItemKind::Network));
    assert_eq!(net[0].name, "\\\\SERVER\\share");
}

#[test]
fn links_with_local_paths() {
    let b = LnkBuilder {
        created: FT,
        accessed: FT + 10_000_000,
        modified: FT + 20_000_000,
        file_size: 4096,
        attributes: 0x20,
        id_list: Some(sample_ids()),
        local_base_path: Some("C:\\Documents and Settings\\Mr. Evil\\".into()),
        common_path_suffix: "hacking notes.txt".into(),
        volume_label: "SYSTEM".into(),
        drive_serial: 0xDEAD_BEEF,
        relative_path: Some("..\\..\\hacking notes.txt".into()),
        working_dir: Some("C:\\Documents and Settings\\Mr. Evil".into()),
        tracker: Some((
            "n-1a9odn6zxk4lq".into(),
            "5D5B5C8A-F1C2-11D8-9A38-0010A4933E09".into(),
        )),
        environment_target: Some("%USERPROFILE%\\hacking notes.txt".into()),
        ..LnkBuilder::default()
    };
    let bytes = b.build();
    let l = ShellLink::parse(&bytes).unwrap();
    assert_eq!(l.len, bytes.len());
    assert_eq!(l.created_unix(), Some(FT_UNIX));
    assert_eq!(l.accessed_unix(), Some(FT_UNIX + 1));
    assert_eq!(l.modified_unix(), Some(FT_UNIX + 2));
    assert_eq!(l.file_size, 4096);
    assert_eq!(
        l.best_path(),
        "C:\\Documents and Settings\\Mr. Evil\\hacking notes.txt"
    );
    let li = l.link_info.as_ref().unwrap();
    assert_eq!(li.volume_label.as_deref(), Some("SYSTEM"));
    assert_eq!(li.drive_serial, Some(0xDEAD_BEEF));
    assert_eq!(li.drive_type, Some(3));
    assert_eq!(
        l.relative_path.as_deref(),
        Some("..\\..\\hacking notes.txt")
    );
    let t = l.extra.tracker.as_ref().unwrap();
    assert_eq!(t.machine_id, "n-1a9odn6zxk4lq");
    assert_eq!(t.mac_address.as_deref(), Some("00:10:a4:93:3e:09"));
    assert_eq!(
        l.extra.environment_target.as_deref(),
        Some("%USERPROFILE%\\hacking notes.txt")
    );
    assert_eq!(l.id_list.len(), 5);

    // Unicode paths take precedence (Autopsy's best-path order).
    let u = LnkBuilder {
        local_base_path: Some("D:\\Ünï\\".into()),
        unicode_paths: true,
        common_path_suffix: "file.doc".into(),
        ..LnkBuilder::default()
    };
    assert_eq!(
        ShellLink::parse(&u.build()).unwrap().best_path(),
        "D:\\Ünï\\file.doc"
    );
}

#[test]
fn links_to_network_shares_and_id_list_only() {
    let n = LnkBuilder {
        net_name: Some("\\\\FILESRV\\public".into()),
        common_path_suffix: "plans\\q3.xls".into(),
        ..LnkBuilder::default()
    };
    assert_eq!(
        ShellLink::parse(&n.build()).unwrap().best_path(),
        "\\\\FILESRV\\public\\plans\\q3.xls"
    );
    let i = LnkBuilder {
        id_list: Some(sample_ids()),
        ..LnkBuilder::default()
    };
    assert_eq!(
        ShellLink::parse(&i.build()).unwrap().best_path(),
        "C:\\Documents and Settings\\Mr. Evil\\hacking notes.txt"
    );
}

#[test]
fn rejects_non_links() {
    assert!(matches!(ShellLink::parse(b"hello"), Err(Error::NotLink)));
    let mut b = LnkBuilder::default().build();
    b.truncate(0x4C);
    assert!(
        ShellLink::parse(&b).is_ok(),
        "header only (flags without data)"
    );
}

fn link(path: &str) -> Vec<u8> {
    let (dir, name) = path.rsplit_once('\\').unwrap();
    LnkBuilder {
        created: FT,
        local_base_path: Some(format!("{dir}\\")),
        common_path_suffix: name.into(),
        ..LnkBuilder::default()
    }
    .build()
}

#[test]
fn automatic_destinations_with_dest_list() {
    for win10 in [false, true] {
        let big = {
            let mut l = LnkBuilder {
                local_base_path: Some("C:\\big\\".into()),
                common_path_suffix: "file.bin".into(),
                description: Some("x".repeat(3000)),
                ..LnkBuilder::default()
            };
            l.arguments = Some("--flag".into());
            l.build()
        };
        assert!(big.len() > 4096, "goes into regular sectors");
        let dl = dest_list(
            &[
                DestEntry {
                    entry_number: 1,
                    hostname: "n-1a9odn6zxk4lq",
                    last_access: FT,
                    pinned: false,
                    access_count: 4,
                    path: "C:\\docs\\a.doc",
                },
                DestEntry {
                    entry_number: 0xa,
                    hostname: "other",
                    last_access: FT + 10_000_000,
                    pinned: true,
                    access_count: 1,
                    path: "C:\\big\\file.bin",
                },
            ],
            win10,
        );
        let cf = compound_file(&[
            ("1", link("C:\\docs\\a.doc")),
            ("a", big.clone()),
            ("DestList", dl),
            ("junk", b"not a link".to_vec()),
        ]);
        let j = parse_automatic(&cf).unwrap();
        assert_eq!(j.dest_list_version, Some(if win10 { 3 } else { 1 }));
        assert_eq!(j.dest_list.len(), 2);
        assert_eq!(j.entries.len(), 2);
        assert_eq!(j.unparsed_streams, vec!["junk"]);
        let a = &j.entries[0];
        assert_eq!(a.stream_name, "1");
        assert_eq!(a.link.best_path(), "C:\\docs\\a.doc");
        let d = a.dest.as_ref().unwrap();
        assert_eq!(d.hostname, "n-1a9odn6zxk4lq");
        assert_eq!(d.last_access_unix(), Some(FT_UNIX));
        assert!(!d.pinned);
        assert_eq!(d.access_count, if win10 { Some(4) } else { None });
        assert_eq!(d.path, "C:\\docs\\a.doc");
        let b = &j.entries[1];
        assert_eq!(b.stream_name, "a");
        assert_eq!(b.link.best_path(), "C:\\big\\file.bin");
        assert!(b.dest.as_ref().unwrap().pinned);
    }
    assert!(matches!(
        parse_automatic(b"nope"),
        Err(Error::NotCompoundFile)
    ));
}

#[test]
fn custom_destinations_and_app_ids() {
    let f = custom_destinations(&[link("C:\\x\\one.txt"), link("D:\\two.pdf")]);
    let links = parse_custom(&f);
    assert_eq!(links.len(), 2);
    assert_eq!(links[1].best_path(), "D:\\two.pdf");
    assert_eq!(
        app_id("5d696d521de238c3.customDestinations-ms"),
        "5d696d521de238c3"
    );
    assert_eq!(app_name("5D696D521DE238C3"), Some("Google Chrome"));
    assert_eq!(app_name("0000"), None);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn links_never_panic(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let mut d = link("C:\\a\\b.txt");
        let n = d.len().min(data.len());
        d[..n].copy_from_slice(&data[..n]);
        d[..20].copy_from_slice(&link("C:\\x")[..20]);
        if let Ok(l) = ShellLink::parse(&d) {
            let _ = l.best_path();
        }
        let _ = ShellLink::parse(&data);
        let _ = parse_id_list(&data);
        let _ = parse_custom(&data);
    }

    #[test]
    fn compound_files_never_panic(flips in proptest::collection::vec((0usize..6000, any::<u8>()), 1..40)) {
        let mut cf = compound_file(&[
            ("1", link("C:\\docs\\a.doc")),
            ("DestList", dest_list(&[DestEntry { entry_number: 1, hostname: "h", last_access: FT, pinned: false, access_count: 1, path: "p" }], true)),
        ]);
        for (at, v) in flips {
            let n = cf.len();
            cf[at % n] = v;
        }
        let _ = parse_automatic(&cf);
    }
}

/// A Windows 10 (version 4) `DestList` entry as Windows wrote it on
/// DESKTOP-SDN1RPT: a float at 0x60 before the FILETIME at 0x64.
#[test]
fn windows10_dest_list_times() {
    let mut entry = vec![0u8; 0x82];
    entry[0x48..0x57].copy_from_slice(b"desktop-sdn1rpt");
    entry[0x58..0x80].copy_from_slice(&[
        0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x3F, 0xD5, 0xBC, 0x28, 0x84, 0x10, 0x8E, 0xD6,
        0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0,
    ]);
    let path = "C:\\Users\\mortysmith\\Documents\\Portal_gun.png";
    entry[0x80..0x82].copy_from_slice(&(path.len() as u16).to_le_bytes());
    for c in path.encode_utf16() {
        entry.extend(c.to_le_bytes());
    }
    entry.extend([0u8; 4]);
    let mut stream = vec![0u8; 32];
    stream[..4].copy_from_slice(&4u32.to_le_bytes());
    stream[4..8].copy_from_slice(&1u32.to_le_bytes());
    stream.extend(entry);
    let (version, entries) = pm_lnk::jumplist::parse_dest_list(&stream);
    assert_eq!(version, Some(4));
    let e = &entries[0];
    assert_eq!(e.entry_number, 5);
    assert_eq!(e.path, path);
    assert!(!e.pinned);
    assert_eq!(e.access_count, Some(1));
    // 2020-09-18T23:07:44Z
    assert_eq!(e.last_access_unix(), Some(1_600_470_464));
}

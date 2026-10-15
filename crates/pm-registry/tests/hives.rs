#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::ops::ControlFlow;
use std::sync::Arc;

use pm_core::SharedSource;
use pm_core::io::MemSource;
use pm_registry::builder::{
    DEFAULT_FILETIME, HiveBuilder, ListKind, new_format_log, old_format_log, set_sequence,
};
use pm_registry::{Error, Hive, ValueType};
use proptest::prelude::*;

fn src(b: Vec<u8>) -> SharedSource {
    Arc::new(MemSource::new(b))
}

fn sample(kind: ListKind) -> Vec<u8> {
    let mut b = HiveBuilder::new("CMI-CreateHive{2A7FB991}").list_kind(kind);
    b.sz(
        "Microsoft\\Windows NT\\CurrentVersion",
        "ProductName",
        "Microsoft Windows XP",
    )
    .sz(
        "Microsoft\\Windows NT\\CurrentVersion",
        "CSDVersion",
        "Service Pack 2",
    )
    .dword(
        "Microsoft\\Windows NT\\CurrentVersion",
        "InstallDate",
        1_093_042_107,
    )
    .qword(
        "Microsoft\\Windows NT\\CurrentVersion",
        "InstallTime",
        0x01D2_0000_0000_0000,
    )
    .expand_sz(
        "Microsoft\\Windows NT\\CurrentVersion",
        "SystemRoot",
        "%SystemDrive%\\WINDOWS",
    )
    .multi_sz("Classes", "Multi", &["one", "two"])
    .binary("Classes", "", &[1, 2, 3])
    .value("Classes", "BE", ValueType::DwordBigEndian, &[0, 0, 1, 0])
    .key_time("Classes\\Ünïcödé", DEFAULT_FILETIME + 10_000_000)
    .class("Classes", "MyClass");
    for i in 0..40 {
        b.key(&format!("Many\\Sub{i:03}"));
    }
    b.binary("Big", "Blob", &vec![0xAB; 50_000]);
    b.build()
}

#[test]
fn keys_values_and_lists_of_every_kind() {
    for kind in [ListKind::Li, ListKind::Lf, ListKind::Lh, ListKind::Ri] {
        let hive = Hive::open(src(sample(kind))).unwrap();
        assert!(!hive.header().is_dirty());
        let root = hive.root().unwrap();
        assert!(root.is_root());
        assert_eq!(root.name(), "CMI-CreateHive{2A7FB991}");
        let cv = hive
            .key("microsoft/windows nt\\CURRENTVERSION")
            .unwrap()
            .expect("lookup is case-insensitive");
        assert_eq!(cv.path(), "Microsoft\\Windows NT\\CurrentVersion");
        assert_eq!(
            cv.string_value("productname").unwrap(),
            "Microsoft Windows XP"
        );
        assert_eq!(cv.dword_value("InstallDate"), Some(1_093_042_107));
        let it = cv.value("InstallTime").unwrap().unwrap();
        assert_eq!(it.data_type(), ValueType::Qword);
        assert_eq!(it.qword(), Some(0x01D2_0000_0000_0000));
        assert_eq!(
            cv.string_value("SystemRoot").unwrap(),
            "%SystemDrive%\\WINDOWS"
        );
        assert_eq!(cv.parent().unwrap().name(), "Windows NT");
        assert_eq!(cv.last_written_filetime(), DEFAULT_FILETIME);

        let classes = hive.key("Classes").unwrap().unwrap();
        assert_eq!(classes.class_name().as_deref(), Some("MyClass"));
        let m = classes.value("Multi").unwrap().unwrap();
        assert_eq!(m.multi_string().unwrap(), vec!["one", "two"]);
        assert_eq!(m.string().unwrap(), "one, two");
        assert_eq!(classes.binary_value("").unwrap(), vec![1, 2, 3]);
        assert_eq!(classes.dword_value("BE"), Some(256));
        let uni = classes.subkey("ÜNÏCÖDÉ").unwrap().unwrap();
        assert_eq!(uni.name(), "Ünïcödé");
        assert_eq!(uni.last_written_unix(), Some(1_614_853_231));
        assert!(classes.security_descriptor().is_some());

        let many = hive.key("Many").unwrap().unwrap();
        let subs = many.subkeys().unwrap();
        assert_eq!(subs.len(), 40, "{kind:?}");
        assert_eq!(subs[0].name(), "Sub000");
        assert!(many.subkey("sub039").unwrap().is_some());
        assert!(many.subkey("sub040").unwrap().is_none());
        assert!(hive.key("Nope\\Nothing").unwrap().is_none());

        let big = hive
            .key("Big")
            .unwrap()
            .unwrap()
            .binary_value("Blob")
            .unwrap();
        assert_eq!(big.len(), 50_000);
        assert!(big.iter().all(|&b| b == 0xAB));

        let mut n = 0;
        hive.walk(usize::MAX, |_| {
            n += 1;
            ControlFlow::Continue(())
        });
        assert_eq!(n, 1 + 3 + 2 + 41 + 1);
    }
}

#[test]
fn minor_version_3_stores_large_data_in_one_cell() {
    let mut b = HiveBuilder::new("ROOT").minor_version(3);
    b.binary("K", "V", &vec![7u8; 20_000]);
    let hive = Hive::open(src(b.build())).unwrap();
    let v = hive.key("K").unwrap().unwrap().binary_value("V").unwrap();
    assert_eq!(v.len(), 20_000);
}

#[test]
fn rejects_non_hives() {
    assert!(matches!(
        Hive::open(src(vec![0; 8192])),
        Err(Error::NotRegf)
    ));
    assert!(Hive::open(src(b"regf".to_vec())).is_err());
}

#[test]
fn damaged_root_offset_falls_back_to_first_cell() {
    let mut h = sample(ListKind::Lh);
    h[36..40].copy_from_slice(&0x1234u32.to_le_bytes());
    let hive = Hive::open(src(h)).unwrap();
    assert!(!hive.header().checksum_ok);
    assert!(hive.key("Classes").unwrap().is_some());
}

#[test]
fn deleted_keys_are_recovered_from_free_cells() {
    let mut b = HiveBuilder::new("ROOT");
    b.sz("Software\\Keep", "A", "kept");
    b.sz("Software\\Gone", "Secret", "evidence")
        .dword("Software\\Gone", "Count", 7)
        .delete("Software\\Gone");
    let hive = Hive::open(src(b.build())).unwrap();
    assert!(hive.key("Software\\Gone").unwrap().is_none());
    let del = hive.deleted_keys(100);
    let gone = del.iter().find(|k| k.name == "Gone").expect("recovered");
    assert_eq!(gone.parent_path.as_deref(), Some("Software"));
    assert_eq!(gone.last_written, DEFAULT_FILETIME);
    assert_eq!(gone.values.len(), 2);
    let secret = gone.values.iter().find(|v| v.name == "Secret").unwrap();
    assert_eq!(
        pm_registry::decode_sz(secret.data.as_ref().unwrap()),
        "evidence"
    );
    assert_eq!(
        gone.values[1].data.as_deref(),
        Some(&7u32.to_le_bytes()[..])
    );
    assert!(!del.iter().any(|k| k.name == "Keep"));
}

/// A stale primary file plus the hive it should become.
fn stale_and_updated() -> (Vec<u8>, Vec<u8>) {
    let mut b = HiveBuilder::new("ROOT");
    b.sz("Software\\App", "Version", "1.0");
    for i in 0..30 {
        b.sz(&format!("Filler\\K{i}"), "Pad", &"x".repeat(200));
    }
    let stale = b.build();
    b.sz("Software\\App", "Version", "2.0")
        .sz("Software\\NewKey", "Added", "after the last flush");
    for i in 0..40 {
        b.sz(&format!("Growth\\K{i}"), "Pad", &"y".repeat(300));
    }
    (stale, b.build())
}

#[test]
fn new_format_logs_replay_dirty_hives() {
    let (mut stale, updated) = stale_and_updated();
    assert!(updated.len() > stale.len(), "the hive grows");
    set_sequence(&mut stale, 6, 5);
    let log1 = new_format_log(&stale, &updated, 5, 3);

    // Without logs: the stale content.
    let hive = Hive::open(src(stale.clone())).unwrap();
    assert!(hive.header().is_dirty());
    assert_eq!(
        hive.key("Software\\App")
            .unwrap()
            .unwrap()
            .string_value("Version")
            .unwrap(),
        "1.0"
    );

    // With an unrelated junk log and the real one.
    let junk = src(vec![0x55; 2048]);
    let hive = Hive::open_with_logs(src(stale.clone()), &[junk, src(log1.clone())]).unwrap();
    let r = hive.replay_report();
    assert!(r.new_format_entries >= 2, "{r:?}");
    assert_eq!(r.ignored_logs, 1);
    assert!(!hive.header().is_dirty());
    assert_eq!(
        hive.key("Software\\App")
            .unwrap()
            .unwrap()
            .string_value("Version")
            .unwrap(),
        "2.0"
    );
    assert!(hive.key("Software\\NewKey").unwrap().is_some());
    assert_eq!(
        hive.key("Growth")
            .unwrap()
            .unwrap()
            .subkeys()
            .unwrap()
            .len(),
        40
    );

    // A corrupted entry stops replay at the previous one.
    let mut bad = log1;
    let second = 512 + u32::from_le_bytes(bad[516..520].try_into().unwrap()) as usize;
    bad[second + 100] ^= 0xFF;
    let hive = Hive::open_with_logs(src(stale.clone()), &[src(bad)]).unwrap();
    assert_eq!(hive.replay_report().new_format_entries, 1);

    // Entries older than the hive's sequence number are skipped.
    let old_log = new_format_log(&stale, &updated, 1, 100);
    let hive = Hive::open_with_logs(src(stale), &[src(old_log)]).unwrap();
    assert!(!hive.replay_report().applied());
}

#[test]
fn clean_hives_ignore_logs() {
    let (stale, updated) = stale_and_updated();
    let log = new_format_log(&stale, &updated, 1, 100);
    let hive = Hive::open_with_logs(src(stale), &[src(log)]).unwrap();
    assert!(!hive.replay_report().applied());
    assert!(hive.key("Software\\NewKey").unwrap().is_none());
}

#[test]
fn old_format_logs_replay_dirty_hives() {
    let (mut stale, updated) = stale_and_updated();
    set_sequence(&mut stale, 2, 1);
    let mut upd = updated.clone();
    set_sequence(&mut upd, 2, 2);
    let log = old_format_log(&stale, &upd);
    let hive = Hive::open_with_logs(src(stale), &[src(log)]).unwrap();
    assert!(hive.replay_report().old_format_sectors > 0);
    assert!(!hive.header().is_dirty());
    assert_eq!(
        hive.key("Software\\App")
            .unwrap()
            .unwrap()
            .string_value("Version")
            .unwrap(),
        "2.0"
    );
    assert_eq!(
        hive.key("Growth")
            .unwrap()
            .unwrap()
            .subkeys()
            .unwrap()
            .len(),
        40
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn random_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..10_000)) {
        let mut d = data;
        if d.len() >= 4 {
            d[..4].copy_from_slice(b"regf");
        }
        exercise(d);
    }

    #[test]
    fn mutated_hives_never_panic(flips in proptest::collection::vec((0usize..20_000, any::<u8>()), 1..64)) {
        let mut h = sample(ListKind::Ri);
        for (at, v) in flips {
            let n = h.len();
            h[4096 + at % (n - 4096)] = v;
        }
        exercise(h);
    }
}

fn exercise(d: Vec<u8>) {
    let log = d.clone();
    if let Ok(h) = Hive::open_with_logs(src(d), &[src(log)]) {
        h.walk(10_000, |k| {
            let _ = k.name();
            let _ = k.path();
            let _ = k.class_name();
            for v in k.values().unwrap_or_default() {
                let _ = v.string();
                let _ = v.qword();
            }
            ControlFlow::Continue(())
        });
        let _ = h.deleted_keys(1000);
    }
}

/// Walk throughput on a large synthetic hive (`cargo test --release -p
/// pm-registry -- --ignored --nocapture bench_`).
#[test]
#[ignore = "benchmark"]
fn bench_walk_large_hive() {
    let mut b = HiveBuilder::new("ROOT");
    for i in 0..200_000u32 {
        b.sz(
            &format!("A{}\\B{}\\K{i}", i % 50, i % 997),
            "V",
            "some value data",
        );
    }
    let bytes = b.build();
    let size = bytes.len();
    let t = std::time::Instant::now();
    let hive = Hive::open(src(bytes)).unwrap();
    let mut keys = 0u64;
    let mut values = 0u64;
    hive.walk(usize::MAX, |k| {
        keys += 1;
        for v in k.values().unwrap() {
            values += u64::from(v.string().is_some());
        }
        ControlFlow::Continue(())
    });
    let dt = t.elapsed();
    eprintln!(
        "walked {keys} keys / {values} values of a {:.1} MB hive in {dt:?} ({:.0} keys/s)",
        size as f64 / 1e6,
        keys as f64 / dt.as_secs_f64()
    );
    assert!(keys > 200_000);
}

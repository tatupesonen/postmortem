#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use pm_prefetch::builder::{PrefetchBuilder, compress_mam, xpress_compress};
use pm_prefetch::{parse, xpress};
use proptest::prelude::*;

/// 2004-08-27 01:26:44 UTC.
const T1: u64 = 127_380_436_040_000_000;
const T1_UNIX: i64 = 1_093_570_004;

fn sample(version: u32, short: bool) -> PrefetchBuilder {
    PrefetchBuilder {
        version,
        executable: "PING.EXE".into(),
        hash: 0x31C1_5F1E,
        run_count: 7,
        run_times: vec![T1, T1 - 36_000_000_000, 0, T1 - 72_000_000_000],
        filenames: vec![
            "\\DEVICE\\HARDDISKVOLUME1\\WINDOWS\\SYSTEM32\\NTDLL.DLL".into(),
            "\\DEVICE\\HARDDISKVOLUME1\\WINDOWS\\SYSTEM32\\PING.EXE".into(),
        ],
        volumes: vec![(
            "\\DEVICE\\HARDDISKVOLUME1".into(),
            T1 - 1_000_000_000_000,
            0x0C4A_DF04,
            vec![
                "\\DEVICE\\HARDDISKVOLUME1\\WINDOWS\\".into(),
                "\\DEVICE\\HARDDISKVOLUME1\\WINDOWS\\SYSTEM32\\".into(),
            ],
        )],
        v30_short: short,
    }
}

#[test]
fn every_version_parses() {
    for (version, short) in [
        (17, false),
        (23, false),
        (26, false),
        (30, false),
        (30, true),
        (31, false),
    ] {
        let p = parse(&sample(version, short).build()).unwrap();
        assert_eq!(p.version, version);
        assert!(!p.compressed);
        assert_eq!(p.executable, "PING.EXE");
        assert_eq!(p.hash, 0x31C1_5F1E);
        assert_eq!(p.run_count, 7, "v{version} short={short}");
        let times = p.last_run_unix();
        assert_eq!(times[0], T1_UNIX);
        let want = if version >= 26 { 3 } else { 1 };
        assert_eq!(times.len(), want, "v{version}: zero slots skipped");
        assert_eq!(p.filenames.len(), 2);
        assert_eq!(p.executable_dir(), "/WINDOWS/SYSTEM32");
        let v = &p.volumes[0];
        assert_eq!(v.device_path, "\\DEVICE\\HARDDISKVOLUME1");
        assert_eq!(v.serial, 0x0C4A_DF04);
        assert_eq!(v.directories.len(), 2);
    }
}

#[test]
fn compressed_windows10_prefetch() {
    let plain = sample(30, false).build();
    for crc in [false, true] {
        let mam = compress_mam(&plain, crc);
        let p = parse(&mam).unwrap();
        assert!(p.compressed);
        assert_eq!(p.executable, "PING.EXE");
        assert_eq!(p.run_count, 7);
    }
}

#[test]
fn xpress_roundtrips_long_matches_and_blocks() {
    // > 64 KiB with long runs (extra length bytes, incl. the 16-bit form).
    let mut data = Vec::new();
    for i in 0..3000u32 {
        data.extend(format!("line {i} of some repetitive text; ").as_bytes());
    }
    data.extend(vec![b'z'; 70_000]);
    data.extend((0..5000u32).map(|i| (i * 7 % 251) as u8));
    let c = xpress_compress(&data);
    assert!(c.len() < data.len() / 2);
    assert_eq!(xpress::decompress(&c, data.len()).unwrap(), data);
    assert_eq!(
        xpress::decompress(&xpress_compress(b"ab"), 2).unwrap(),
        b"ab"
    );
}

#[test]
fn xpress_rejects_garbage() {
    assert!(xpress::decompress(&[0u8; 10], 5).is_err(), "short table");
    // A table with no codes at all.
    assert!(xpress::decompress(&[0u8; 300], 5).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn xpress_roundtrip(data in proptest::collection::vec(0u8..4, 0..3000)) {
        let c = xpress_compress(&data);
        prop_assert_eq!(xpress::decompress(&c, data.len()).unwrap(), data);
    }

    #[test]
    fn never_panics(data in proptest::collection::vec(any::<u8>(), 0..4000), size in 0usize..100_000) {
        let _ = xpress::decompress(&data, size);
        let _ = parse(&data);
        let mut pf = sample(30, false).build();
        let n = pf.len().min(data.len());
        if n > 8 {
            pf[8..n].copy_from_slice(&data[8..n]);
        }
        let _ = parse(&pf);
        let mut mam = b"MAM\x04".to_vec();
        mam.extend((size as u32).to_le_bytes());
        mam.extend(&data);
        let _ = parse(&mam);
    }
}

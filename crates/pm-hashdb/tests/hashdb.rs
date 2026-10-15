#![allow(
    clippy::unwrap_used,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

use pm_hashdb::{Format, HashDb, HashDbError, detect, index_path, parse_md5_hex};
use proptest::prelude::*;

fn md5_of(i: u64) -> [u8; 16] {
    let mut m = [0u8; 16];
    // Spread values over the key space.
    m[..8].copy_from_slice(&i.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_be_bytes());
    m[8..].copy_from_slice(&i.to_be_bytes());
    m
}

fn hex(m: &[u8; 16]) -> String {
    m.iter().map(|b| format!("{b:02x}")).collect()
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

fn indexed(p: &Path) -> HashDb {
    assert!(matches!(
        HashDb::open(p, true),
        Err(HashDbError::NeedsIndex(_))
    ));
    HashDb::build_index(p, &AtomicU64::new(0)).unwrap();
    HashDb::open(p, true).unwrap()
}

#[test]
fn kdb_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.kdb");
    {
        let db = HashDb::create(&p).unwrap();
        assert!(db.is_updateable());
        db.add(
            &md5_of(1),
            None,
            Some(&[7; 32]),
            Some("evil.exe"),
            Some("dropper"),
        )
        .unwrap();
        db.add(&md5_of(1), None, None, Some("evil2.exe"), Some("dropper"))
            .unwrap();
        db.add(&md5_of(2), None, None, None, None).unwrap();
        assert!(matches!(HashDb::create(&p), Err(HashDbError::Exists(_))));
    }
    assert_eq!(detect(&p).unwrap(), Format::Kdb);
    let db = HashDb::open(&p, true).unwrap();
    assert!(!db.is_updateable());
    assert!(matches!(
        db.add(&md5_of(3), None, None, None, None),
        Err(HashDbError::ReadOnly)
    ));
    assert_eq!(db.len().unwrap(), 2);
    let hit = db.lookup(&md5_of(1)).unwrap().unwrap();
    assert_eq!(hit.names, ["evil.exe", "evil2.exe"]);
    assert_eq!(hit.comments, ["dropper"]);
    assert!(db.contains(&md5_of(2)).unwrap());
    assert!(db.lookup(&md5_of(9)).unwrap().is_none());
    assert_eq!(db.display_name(), "bad");
}

#[test]
fn md5sum_set() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "{}  a.txt\n{} *b.exe\r\nMD5 (c d.jpg) = {}\n\n",
        hex(&md5_of(1)),
        hex(&md5_of(2)),
        hex(&md5_of(3)).to_uppercase()
    );
    let p = write(dir.path(), "set.md5", &body);
    assert_eq!(detect(&p).unwrap(), Format::Md5sum);
    let db = indexed(&p);
    assert_eq!(db.len().unwrap(), 3);
    assert_eq!(db.lookup(&md5_of(2)).unwrap().unwrap().names, ["b.exe"]);
    assert_eq!(db.lookup(&md5_of(3)).unwrap().unwrap().names, ["c d.jpg"]);
    assert!(!db.contains(&md5_of(4)).unwrap());
    assert!(index_path(&p).exists());
}

#[test]
fn nsrl_v2_both_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let cur = format!(
        "\"SHA-1\",\"MD5\",\"CRC32\",\"FileName\",\"FileSize\",\"ProductCode\",\"OpSystemCode\",\"SpecialCode\"\n\
         \"00\",\"{}\",\"00\",\"kernel32.dll\",1,2,\"WIN\",\"\"\n",
        hex(&md5_of(5)).to_uppercase()
    );
    let p = write(dir.path(), "NSRLFile.txt", &cur);
    assert_eq!(detect(&p).unwrap(), Format::NsrlV2);
    assert_eq!(
        indexed(&p).lookup(&md5_of(5)).unwrap().unwrap().names,
        ["kernel32.dll"]
    );

    let old = format!(
        "\"SHA-1\",\"FileName\",\"FileSize\",\"ProductCode\",\"OpSystemCode\",\"MD4\",\"MD5\",\"CRC32\",\"SpecialCode\"\n\
         \"00\",\"old.sys\",1,2,\"WIN\",\"00\",\"{}\",\"00\",\"\"\n",
        hex(&md5_of(6))
    );
    let p = write(dir.path(), "NSRLFile-old.txt", &old);
    assert_eq!(
        indexed(&p).lookup(&md5_of(6)).unwrap().unwrap().names,
        ["old.sys"]
    );
}

#[test]
fn nsrl_v3_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("RDS.db");
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE FILE (sha256 TEXT, sha1 TEXT, md5 TEXT, crc32 TEXT, file_name TEXT, file_size INTEGER, package_id INTEGER);",
        )
        .unwrap();
        for i in 0..50u64 {
            c.execute(
                "INSERT INTO FILE (md5, file_name) VALUES (?1, ?2)",
                rusqlite::params![hex(&md5_of(i)).to_uppercase(), format!("f{i}.bin")],
            )
            .unwrap();
        }
    }
    assert_eq!(detect(&p).unwrap(), Format::NsrlV3);
    let db = indexed(&p);
    assert_eq!(db.len().unwrap(), 50);
    assert_eq!(db.lookup(&md5_of(17)).unwrap().unwrap().names, ["f17.bin"]);
}

#[test]
fn hashkeeper_set() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "\"file_id\",\"hashset_id\",\"file_name\",\"directory\",\"hash\",\"file_size\",\"date_modified\",\"time_modified\",\"time_zone\",\"comments\",\"date_accessed\",\"time_accessed\"\n\
         1,9,\"tool.exe\",\"C:\\\\x\",\"{}\",100,\"\",\"\",\"\",\"hacking tool\",\"\",\"\"\n",
        hex(&md5_of(8))
    );
    let p = write(dir.path(), "set.hsh", &body);
    assert_eq!(detect(&p).unwrap(), Format::HashKeeper);
    let hit = indexed(&p).lookup(&md5_of(8)).unwrap().unwrap();
    assert_eq!(hit.names, ["tool.exe"]);
    assert_eq!(hit.comments, ["hacking tool"]);
}

#[test]
fn encase_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = vec![0u8; 1152];
    b[..8].copy_from_slice(b"HASH\x0d\x0a\xff\x00");
    for (i, u) in "Case 42 bad".encode_utf16().enumerate() {
        b[1032 + i * 2..1034 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    for i in 0..10u64 {
        b.extend_from_slice(&md5_of(i));
        b.extend_from_slice(&[0, 0]);
    }
    let p = dir.path().join("set.hash");
    std::fs::write(&p, &b).unwrap();
    assert_eq!(detect(&p).unwrap(), Format::EnCase);
    let db = indexed(&p);
    assert_eq!(db.display_name(), "Case 42 bad");
    assert!(db.contains(&md5_of(9)).unwrap());
    assert_eq!(
        db.lookup(&md5_of(3)).unwrap().unwrap().names,
        Vec::<String>::new()
    );
}

#[test]
fn tsk_idx_only() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "{}|set.txt\n{}|0000000000\n{}|0000000044\n",
        "0".repeat(41),
        hex(&md5_of(1)),
        hex(&md5_of(2))
    );
    let p = write(dir.path(), "set.txt-md5.idx", &body);
    assert_eq!(detect(&p).unwrap(), Format::TskIdx);
    let db = indexed(&p);
    assert_eq!(db.len().unwrap(), 2);
    assert!(db.contains(&md5_of(2)).unwrap());
}

#[test]
fn stale_index_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(dir.path(), "s.md5", &format!("{}  a\n", hex(&md5_of(1))));
    indexed(&p);
    std::fs::write(
        &p,
        format!("{}  a\n{}  b\n", hex(&md5_of(1)), hex(&md5_of(2))),
    )
    .unwrap();
    assert!(matches!(
        HashDb::open(&p, true),
        Err(HashDbError::NeedsIndex(_))
    ));
    let db = HashDb::open_or_index(&p, &AtomicU64::new(0)).unwrap();
    assert!(db.contains(&md5_of(2)).unwrap());
}

#[test]
fn external_sort_and_duplicates_across_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let mut body = String::new();
    // 700 names for one hash: spans several 256-record index blocks.
    for i in 0..700 {
        body.push_str(&format!("{}  dup{i}\n", hex(&md5_of(42))));
    }
    for i in 0..3000u64 {
        body.push_str(&format!("{}  f{i}\n", hex(&md5_of(i + 1000))));
    }
    let p = write(dir.path(), "big.md5", &body);
    let progress = AtomicU64::new(0);
    // Tiny runs force the k-way merge path.
    let n = HashDb::build_index_chunked(&p, &progress, 97).unwrap();
    assert_eq!(n, 3700);
    assert_eq!(progress.load(std::sync::atomic::Ordering::Relaxed), 3700);
    let db = HashDb::open(&p, true).unwrap();
    let hit = db.lookup(&md5_of(42)).unwrap().unwrap();
    assert_eq!(hit.names.len(), 700);
    for i in (0..3000u64).step_by(37) {
        assert!(db.contains(&md5_of(i + 1000)).unwrap(), "{i}");
    }
    assert!(!db.contains(&md5_of(999_999)).unwrap());
    // No run files are left behind.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".run"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn hex_parsing() {
    assert_eq!(parse_md5_hex(&"Ab".repeat(16)), Some([0xAB; 16]));
    assert_eq!(parse_md5_hex("abc"), None);
    assert_eq!(parse_md5_hex(&"zz".repeat(16)), None);
}

#[test]
fn empty_file_is_not_a_hash_set() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(dir.path(), "empty.md5", "");
    assert!(matches!(detect(&p), Err(HashDbError::UnknownFormat(_))));
}

#[test]
fn unknown_format() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(dir.path(), "x.txt", "hello world\n");
    assert!(matches!(detect(&p), Err(HashDbError::UnknownFormat(_))));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn lookups_agree_with_a_set(
        members in prop::collection::vec(0u64..5_000, 1..1500),
        queries in prop::collection::vec(0u64..5_000, 0..200),
        chunk in 1usize..600,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let body: String = members.iter().map(|i| format!("{}  n{i}\n", hex(&md5_of(*i)))).collect();
        let p = write(dir.path(), "p.md5", &body);
        HashDb::build_index_chunked(&p, &AtomicU64::new(0), chunk).unwrap();
        let db = HashDb::open(&p, true).unwrap();
        let set: BTreeSet<u64> = members.iter().copied().collect();
        for q in queries.iter().chain(members.iter().take(50)) {
            prop_assert_eq!(db.contains(&md5_of(*q)).unwrap(), set.contains(q));
            let dupes = members.iter().filter(|m| *m == q).count();
            let names = db.lookup(&md5_of(*q)).unwrap().map_or(0, |h| h.names.len());
            prop_assert_eq!(names, usize::from(dupes > 0));
        }
    }
}

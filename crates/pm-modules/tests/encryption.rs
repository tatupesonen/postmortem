#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

//! End-to-end: Encryption Detection after File Type Identification, on an
//! NTFS volume holding real protected documents (POI/PDFBox fixtures) and
//! high-entropy files, and on partitioned disks with BitLocker, LUKS and
//! random-data volumes. Ports `EncryptionDetectionTest` (password
//! protection, VeraCrypt-style containers, BitLocker) to generated data.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use pm_casedb::Significance;
use pm_encryption::volume::builder::{bitlocker_vbr, luks_header};
use pm_ingest::{JobSettings, MessageKind, ModuleFactory, ModuleSetting};
use pm_modules::encryption::{EncryptionFactory, EncryptionSettings, suspected_comment};
use pm_modules::file_type::FileTypeFactory;
use pm_testkit::disk::{DiskBuilder, MbrPart};
use pm_testkit::ntfs::NtfsBuilder;

use common::DemoCase;

fn fixture(name: &str) -> Vec<u8> {
    let p: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "../../fixtures/encryption",
        name,
    ]
    .iter()
    .collect();
    std::fs::read(p).unwrap()
}

fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect()
}

const MIB: usize = 1024 * 1024;

fn run(c: &DemoCase, settings: &EncryptionSettings) -> Arc<pm_ingest::Inbox> {
    let job = JobSettings {
        modules: vec![
            ModuleSetting {
                id: "file_type".into(),
                enabled: true,
                settings: serde_json::Value::Null,
            },
            ModuleSetting {
                id: "encryption".into(),
                enabled: true,
                settings: serde_json::to_value(settings).unwrap(),
            },
        ],
        ..JobSettings::default()
    };
    let modules: Vec<Arc<dyn ModuleFactory>> =
        vec![Arc::new(FileTypeFactory), Arc::new(EncryptionFactory)];
    let (summary, inbox) = c.run(&job, modules);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    inbox
}

/// `path → (type, comment)` for every file-level result.
fn flagged(c: &DemoCase) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    for ty in ["KAAMOS_ENCRYPTION_DETECTED", "KAAMOS_ENCRYPTION_SUSPECTED"] {
        for (a, attrs) in c.artifacts(ty) {
            let Some(id) = a.file_id else { continue };
            let path = c.case.db().file(id).unwrap().unwrap().path();
            assert_eq!(
                a.justification.as_deref(),
                Some(attrs["KAAMOS_COMMENT"].as_str())
            );
            let want = if ty == "KAAMOS_ENCRYPTION_DETECTED" {
                Significance::Notable
            } else {
                Significance::LikelyNotable
            };
            assert_eq!(a.significance, Some(want), "{path}");
            out.insert(path, (ty.to_owned(), attrs["KAAMOS_COMMENT"].clone()));
        }
    }
    out
}

fn files_image() -> Vec<u8> {
    let mut b = NtfsBuilder::new(40);
    b.dir("/docs").dir("/c");
    for name in [
        "pw.doc",
        "plain.doc",
        "pw.xls",
        "default-pw.xls",
        "pw.ppt",
        "pw-rc4-40.pdf",
        "pw-aes-256.pdf",
        "owner-only.pdf",
        "plain.pdf",
        "pw-agile.docx",
        "plain.docx",
        "head-testAccess2_encrypted.accdb",
        "head-testAccess2_2000.mdb",
    ] {
        b.file(&format!("/docs/{name}"), &fixture(name));
    }
    // VeraCrypt-style containers: 5 MiB of noise (a multiple of 512), one
    // byte too many, and a small SQLCipher-like database.
    b.file("/c/container.hc", &noise(5 * MIB, 7))
        .file("/c/odd-size.bin", &noise(5 * MIB + 1, 9))
        .file("/c/vault.db", &noise(65_536, 11))
        .file("/c/small.db", &noise(65_535, 13))
        .file(
            "/c/plain.db",
            &[b"SQLite format 3\0".as_slice(), &noise(65_520, 17)].concat(),
        )
        .file("/c/zeros.bin", &vec![0u8; 5 * MIB])
        .file("/pagefile.sys", &noise(5 * MIB, 15));
    b.build()
}

#[test]
fn files_like_autopsy() {
    let c = DemoCase::with_image(&files_image());
    let inbox = run(&c, &EncryptionSettings::default());
    let f = flagged(&c);
    let detected = |p: &str| {
        (
            "KAAMOS_ENCRYPTION_DETECTED".to_owned(),
            "Password protection detected.".to_owned(),
        ) == f[p]
    };
    for p in [
        "/docs/pw.doc",
        "/docs/pw.xls",
        "/docs/pw.ppt",
        "/docs/pw-rc4-40.pdf",
        "/docs/pw-aes-256.pdf",
        "/docs/pw-agile.docx",
        "/docs/head-testAccess2_encrypted.accdb",
    ] {
        assert!(f.contains_key(p) && detected(p), "{p}: {f:?}");
    }
    for p in ["/c/container.hc", "/c/vault.db"] {
        let (ty, comment) = &f[p];
        assert_eq!(ty, "KAAMOS_ENCRYPTION_SUSPECTED");
        assert!(
            comment.starts_with("Suspected encryption due to high entropy (7.99"),
            "{comment}"
        );
    }
    // Not flagged: unprotected documents, Excel's default password, an
    // owner-only PDF, wrong sizes, low entropy, the root page file.
    for p in [
        "/docs/plain.doc",
        "/docs/default-pw.xls",
        "/docs/owner-only.pdf",
        "/docs/plain.pdf",
        "/docs/plain.docx",
        "/docs/head-testAccess2_2000.mdb",
        "/c/odd-size.bin",
        "/c/small.db",
        "/c/plain.db",
        "/c/zeros.bin",
        "/pagefile.sys",
    ] {
        assert!(!f.contains_key(p), "{p} flagged: {:?}", f.get(p));
    }
    assert_eq!(f.len(), 9, "{f:?}");

    // One data message per result.
    let msgs: Vec<_> = inbox
        .since(0)
        .into_iter()
        .filter(|m| m.kind == MessageKind::Data)
        .collect();
    assert_eq!(msgs.len(), 9);
    let m = msgs
        .iter()
        .find(|m| m.subject == "Encryption Suspected Match: container.hc")
        .unwrap();
    assert!(
        m.details
            .starts_with("File: /c/container.hc\nEntropy: 7.99"),
        "{}",
        m.details
    );
    assert!(
        msgs.iter()
            .any(|m| m.subject == "Encryption Detected Match: pw.doc"
                && m.details == "File: /docs/pw.doc")
    );

    // Re-ingest adds no duplicates.
    run(&c, &EncryptionSettings::default());
    assert_eq!(flagged(&c).len(), 9);
}

#[test]
fn settings_change_the_size_rules() {
    let c = DemoCase::with_image(&files_image());
    let s = EncryptionSettings {
        file_size_multiple_enforced: false,
        ..EncryptionSettings::default()
    };
    run(&c, &s);
    assert!(flagged(&c).contains_key("/c/odd-size.bin"));
    // Invalid settings fail start-up with Autopsy's messages.
    for (bad, msg) in [
        (
            EncryptionSettings {
                minimum_entropy: 8.5,
                ..EncryptionSettings::default()
            },
            "Minimum entropy input must be a number between 6.0 and 8.0.",
        ),
        (
            EncryptionSettings {
                minimum_file_size: 0,
                ..EncryptionSettings::default()
            },
            "Minimum file size input must be an integer (in megabytes) of 1 or greater.",
        ),
    ] {
        assert_eq!(bad.validate().unwrap_err().to_string(), msg);
    }
    assert_eq!(
        suspected_comment(7.5),
        "Suspected encryption due to high entropy (7.500000)."
    );
}

/// A disk with an NTFS partition and the given raw partitions.
fn disk(raw: &[Vec<u8>]) -> Vec<u8> {
    let ntfs = NtfsBuilder::new(4).build();
    let sectors = |b: &Vec<u8>| (b.len() / 512) as u32;
    let mut parts = vec![MbrPart::new(0x07, 2048, sectors(&ntfs))];
    let mut next = 2048 + sectors(&ntfs);
    for r in raw {
        parts.push(MbrPart::new(0x07, next, sectors(r)));
        next += sectors(r);
    }
    let mut d = DiskBuilder::new(512, u64::from(next) + 2048);
    d.mbr_table(0, &parts);
    d.write(2048 * 512, &ntfs);
    let mut at = (2048 + sectors(&ntfs)) as usize * 512;
    for r in raw {
        d.write(at, r);
        at += r.len();
    }
    d.build()
}

fn volume_results(c: &DemoCase) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for ty in ["KAAMOS_ENCRYPTION_DETECTED", "KAAMOS_ENCRYPTION_SUSPECTED"] {
        for (a, attrs) in c.artifacts(ty) {
            if a.file_id.is_none() {
                out.push((
                    ty.to_owned(),
                    attrs["KAAMOS_COMMENT"].clone(),
                    attrs["KAAMOS_DESCRIPTION"].clone(),
                ));
            }
        }
    }
    out
}

#[test]
fn bitlocker_volume() {
    let mut vbr = bitlocker_vbr();
    vbr.resize(MIB, 0);
    let c = DemoCase::with_image(&disk(&[vbr]));
    run(&c, &EncryptionSettings::default());
    let v = volume_results(&c);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].0, "KAAMOS_ENCRYPTION_DETECTED");
    assert_eq!(v[0].1, "Bitlocker encryption detected.");
    assert!(v[0].2.starts_with("vol"), "{}", v[0].2);
}

#[test]
fn unformatted_random_volume_and_luks() {
    // VeraCrypt-style: random data, no file system.
    let c = DemoCase::with_image(&disk(&[noise(2 * MIB, 3)]));
    run(&c, &EncryptionSettings::default());
    let v = volume_results(&c);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].0, "KAAMOS_ENCRYPTION_SUSPECTED");
    assert!(
        v[0].1
            .starts_with("Suspected encryption due to high entropy (7.99"),
        "{}",
        v[0].1
    );

    // LUKS before a random volume: only the first is reported.
    let mut luks = luks_header();
    luks.resize(MIB, 0);
    let c = DemoCase::with_image(&disk(&[luks, noise(2 * MIB, 5)]));
    run(&c, &EncryptionSettings::default());
    let v = volume_results(&c);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].1, "LUKS encryption detected.");

    // Zeros are not suspicious; the NTFS volume is never checked.
    let c = DemoCase::with_image(&disk(&[vec![0u8; MIB]]));
    run(&c, &EncryptionSettings::default());
    assert!(volume_results(&c).is_empty());
}

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

//! Hostile input never panics: random corruption of the real fixtures and
//! of generated headers.

use std::path::PathBuf;

use pm_core::io::MemSource;
use pm_encryption::office::{OfficeKind, is_password_protected as office};
use proptest::prelude::*;

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

const FILES: [&str; 8] = [
    "pw.xls",
    "pw.doc",
    "pw.ppt",
    "pw-rc4-128.pdf",
    "pw-aes-256.pdf",
    "plain.pdf",
    "head-testAccess2_encrypted.accdb",
    "head-testAccess2_2000.mdb",
];

fn check_all(data: Vec<u8>) {
    let src = MemSource::new(data);
    for k in [OfficeKind::Word, OfficeKind::Excel, OfficeKind::PowerPoint] {
        let _ = office(&src, k);
    }
    let _ = pm_encryption::pdf::is_password_protected(&src);
    let _ = pm_encryption::access::is_password_protected(&src);
    let _ = pm_encryption::volume::detect(&src_head(&src));
    let _ = pm_encryption::entropy::entropy(&src, &|| false);
}

fn src_head(src: &MemSource) -> Vec<u8> {
    pm_core::ReadAt::read_vec_at(src, 0, 512).unwrap_or_default()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn corrupted_fixtures_never_panic(
        which in 0usize..FILES.len(),
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..48),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut data = fixture(FILES[which]);
        for (i, b) in flips {
            let i = i.index(data.len());
            data[i] = b;
        }
        let keep = cut.index(data.len() + 1).max(data.len() / 3);
        data.truncate(keep);
        check_all(data);
    }

    #[test]
    fn arbitrary_bytes_never_panic(
        prefix in prop::sample::select(vec![
            b"%PDF-1.7\n".to_vec(),
            b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1".to_vec(),
            b"\0\x01\0\0Standard Jet DB\0\x01".to_vec(),
            Vec::new(),
        ]),
        tail in prop::collection::vec(any::<u8>(), 0..4096),
    ) {
        let mut data = prefix;
        data.extend(tail);
        check_all(data);
    }
}

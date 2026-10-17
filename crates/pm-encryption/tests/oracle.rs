#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

//! Real documents written by Apache POI 5.3 and `PDFBox` 3.0.3
//! (`scripts/fixtures/MakeEncrypted.java`), checked against what real
//! Tika 3.0.0 reported for each (`EncryptedDocumentException` or not).

use std::path::PathBuf;

use pm_core::io::FileSource;
use pm_encryption::office::{OfficeKind, is_password_protected as office};
use pm_encryption::pdf::is_password_protected as pdf;
use pm_filetype::{FileClass, FileTypeDetector};

fn fixture(name: &str) -> FileSource {
    let p: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "../../fixtures/encryption",
        name,
    ]
    .iter()
    .collect();
    FileSource::open(&p).unwrap()
}

/// `(file, Tika's MIME type after Autopsy's clean-up, Tika throws
/// EncryptedDocumentException)`.
const ORACLE: &[(&str, &str, bool)] = &[
    ("default-pw.xls", "application/vnd.ms-excel", false),
    ("plain.xls", "application/vnd.ms-excel", false),
    ("pw.xls", "application/vnd.ms-excel", true),
    ("plain.doc", "application/msword", false),
    ("pw.doc", "application/msword", true),
    ("plain.ppt", "application/vnd.ms-powerpoint", false),
    ("pw.ppt", "application/vnd.ms-powerpoint", true),
    ("plain.pdf", "application/pdf", false),
    ("owner-only.pdf", "application/pdf", false),
    ("owner-only-aes-256.pdf", "application/pdf", false),
    ("pw-rc4-40.pdf", "application/pdf", true),
    ("pw-rc4-128.pdf", "application/pdf", true),
    ("pw-aes-128.pdf", "application/pdf", true),
    ("pw-aes-256.pdf", "application/pdf", true),
    (
        "plain.docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        false,
    ),
    ("pw-agile.docx", "application/x-ooxml-protected", true),
    ("pw-standard.docx", "application/x-ooxml-protected", true),
];

/// Tika's Access test databases (first 8 KiB, all the check reads) and
/// what Autopsy's Jackcess code path decides for the full files
/// (`AccessOracle` in `scripts/fixtures/MakeEncrypted.java`).
#[test]
fn access_matches_jackcess() {
    for (name, want) in [
        ("head-testAccess2_encrypted.accdb", true),
        ("head-testAccess2.accdb", false),
        ("head-testAccess2_2000.mdb", false),
        ("head-testAccess_V1997.mdb", false),
    ] {
        let src = fixture(name);
        let det = FileTypeDetector::default();
        let ext = name.rsplit_once('.').unwrap().1;
        assert_eq!(
            det.detect(FileClass::File, name, ext, &src),
            "application/x-msaccess",
            "{name}"
        );
        assert_eq!(
            pm_encryption::access::is_password_protected(&src).unwrap(),
            want,
            "{name}"
        );
    }
}

#[test]
fn matches_tika() {
    let det = FileTypeDetector::default();
    for &(name, mime, encrypted) in ORACLE {
        let src = fixture(name);
        let ext = name.rsplit_once('.').unwrap().1;
        assert_eq!(det.detect(FileClass::File, name, ext, &src), mime, "{name}");
        let got = if let Some(kind) = OfficeKind::from_mime(mime) {
            office(&src, kind).unwrap()
        } else if mime == "application/pdf" {
            pdf(&src).unwrap()
        } else {
            // Autopsy flags protected OOXML by its MIME type alone.
            mime == "application/x-ooxml-protected"
        };
        assert_eq!(got, encrypted, "{name}");
    }
}

//! Synthetic evidence builders for tests.
//!
//! Builders produce small, fully specified byte images so parser tests do not
//! depend on large binary fixtures. Builders may panic on misuse — they are
//! test-only code.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::pedantic,
    reason = "test-only fixture builders: panics on misuse are intended, style lints relaxed"
)]

pub mod archives;
pub mod demo;
pub mod disk;
pub mod evt;
pub mod ext;
pub mod lznt1;
pub mod media;
pub mod ntfs;
pub mod synth;
pub mod tsk;

pub use disk::{DiskBuilder, GptPart, MbrPart};

/// Writes generated test data to a file shared by every test process and
/// returns its path: `<temp>/pm-testkit/<stem>-<content hash>.<ext>`.
///
/// The file is written once (atomically, via a rename) and reused, so test
/// binaries neither regenerate nor leak a copy per run (a `TempDir` in a
/// `static` is never dropped). Copies of older contents are removed after
/// a day. Callers must only read the file.
///
/// # Panics
/// If the temporary folder cannot be written.
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test helper")]
pub fn shared_file(stem: &str, ext: &str, bytes: &[u8]) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let dir = std::env::temp_dir().join("pm-testkit");
    std::fs::create_dir_all(&dir).expect("create the shared test-data folder");
    let name = format!("{stem}-{:016x}.{ext}", h.finish());
    let path = dir.join(&name);
    if !path.exists() {
        let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes).expect("write shared test data");
        std::fs::rename(&tmp, &path).expect("publish shared test data");
    }
    let day = std::time::Duration::from_hours(24);
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if stale && n != name && n.starts_with(&format!("{stem}-")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    path
}

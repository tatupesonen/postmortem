#![no_main]
//! Container signature detection (with zip/OLE refinement and the ISO
//! probe) on arbitrary bytes must never panic.

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;

fuzz_target!(|data: &[u8]| {
    let _ = pm_archive::detect_head(data);
    let _ = pm_archive::detect(&MemSource::new(data.to_vec()));
});

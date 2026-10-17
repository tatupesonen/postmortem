#![no_main]
//! File type detection on arbitrary bytes: Autopsy's rules, Tika's magic,
//! XML roots, text statistics and whichever container detector the
//! bytes happen to route to.

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;
use pm_filetype::{FileClass, FileTypeDetector};

fuzz_target!(|data: &[u8]| {
    let src = MemSource::new(data.to_vec());
    let d = FileTypeDetector::default();
    let _ = d.detect(FileClass::File, "fuzz.bin", "bin", &src);
    let _ = pm_filetype::tika_detect(&src, Some("fuzz.docx"));
});

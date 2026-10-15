#![no_main]
//! Prefetch files (plain and MAM-compressed) from arbitrary bytes.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(p) = pm_prefetch::parse(data) {
        let _ = (p.executable_dir(), p.last_run_unix());
    }
    if data.len() > 4 {
        let size = usize::from(data[0]) << 8 | usize::from(data[1]);
        let _ = pm_prefetch::xpress::decompress(&data[2..], size);
    }
});

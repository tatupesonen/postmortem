#![no_main]
//! Opening and fully reading an arbitrary single-segment EWF image must never
//! panic, hang or allocate unboundedly.

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pm_core::ReadAt;
use pm_core::io::MemSource;
use pm_image::ewf::EwfSource;

fuzz_target!(|data: &[u8]| {
    let seg = Arc::new(MemSource::new(data.to_vec()));
    if let Ok((src, _meta)) = EwfSource::from_sources(vec![seg]) {
        let n = src.size().min(4 << 20) as usize;
        let _ = src.read_vec_at(0, n);
    }
});

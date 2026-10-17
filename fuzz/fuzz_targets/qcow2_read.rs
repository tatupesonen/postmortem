#![no_main]
//! Opening an arbitrary qcow2 image and reading it (bounded) must never
//! panic, hang or allocate unboundedly.

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;
use pm_core::{ReadAt, SharedSource};

fuzz_target!(|data: &[u8]| {
    let src: SharedSource = Arc::new(MemSource::new(data.to_vec()));
    let parent: SharedSource = Arc::new(MemSource::new(vec![0x5A; 4096]));
    if let Ok(img) = pm_image::qcow2::from_source(src, Some(parent)).map(|(s, _)| Box::new(s) as Box<dyn ReadAt>) {
        let size = img.size();
        let mut buf = vec![0u8; 65_536];
        let mut off = 0u64;
        while off < size.min(4 << 20) {
            match img.read_at(off, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => off += n as u64,
            }
        }
        let _ = img.read_at(size.saturating_sub(100), &mut buf);
    }
});

#![no_main]
//! Opening, walking and reading an arbitrary exFAT volume (plus the orphan
//! scan) must never panic or hang.

use std::ops::ControlFlow;
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;
use pm_core::{ReadAt, SharedSource};
use pm_fs::FileSystem;
use pm_fs::walk::{WalkOptions, walk};
use pm_fs_exfat::{ExFatFs, ExFatOptions};

fuzz_target!(|data: &[u8]| {
    let src: SharedSource = Arc::new(MemSource::new(data.to_vec()));
    let Ok(fs) = ExFatFs::open(src, &ExFatOptions::default()) else { return };
    walk(&fs, &WalkOptions { max_depth: 32, ..Default::default() }, |i| {
        if let Ok(s) = fs.open(i.entry.inode, None) {
            let _ = s.read_vec_at(0, s.size().min(1 << 16) as usize);
        }
        ControlFlow::Continue(())
    });
    let _ = fs.orphans();
    let _ = fs.unallocated_blocks();
});

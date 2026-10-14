#![no_main]
//! Opening, walking and reading every file of an arbitrary FAT image must
//! never panic or hang.

use std::ops::ControlFlow;
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;
use pm_core::{ReadAt, SharedSource};
use pm_fs::FileSystem;
use pm_fs::walk::{WalkOptions, walk};
use pm_fs_fat::{FatFs, FatOptions};

fuzz_target!(|data: &[u8]| {
    let src: SharedSource = Arc::new(MemSource::new(data.to_vec()));
    let Ok(fs) = FatFs::open(src, &FatOptions::default()) else { return };
    walk(&fs, &WalkOptions::default(), |item| {
        if let Ok(s) = fs.open(item.entry.inode, None) {
            let n = s.size().min(1 << 16) as usize;
            let _ = s.read_vec_at(0, n);
        }
        ControlFlow::Continue(())
    });
    let _ = fs.unallocated_blocks();
});

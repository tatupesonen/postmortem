#![no_main]
//! Opening, walking and reading every stream of an arbitrary NTFS volume
//! (plus orphan and unallocated scans) must never panic or hang.

use std::ops::ControlFlow;
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;
use pm_core::{ReadAt, SharedSource};
use pm_fs::FileSystem;
use pm_fs::walk::{WalkOptions, walk};
use pm_fs_ntfs::NtfsFs;

fuzz_target!(|data: &[u8]| {
    let src: SharedSource = Arc::new(MemSource::new(data.to_vec()));
    let Ok(fs) = NtfsFs::open(src) else { return };
    walk(&fs, &WalkOptions { max_depth: 32, ..Default::default() }, |item| {
        if let Ok(streams) = fs.streams(item.entry.inode) {
            for s in streams {
                if let Ok(c) = fs.open(item.entry.inode, Some(s.id)) {
                    let _ = c.read_vec_at(0, c.size().min(1 << 16) as usize);
                }
            }
        }
        ControlFlow::Continue(())
    });
    let _ = fs.orphans();
    let _ = fs.unallocated_blocks();
});

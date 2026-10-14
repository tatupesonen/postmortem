#![no_main]
//! Partition-table detection must never panic and must yield a consistent
//! layout for arbitrary disks.

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;

fuzz_target!(|data: &[u8]| {
    let src = MemSource::new(data.to_vec());
    if let Ok(Some(vs)) = pm_volume::detect(&src) {
        for w in vs.partitions.windows(2) {
            assert!(w[0].start_sector <= w[1].start_sector);
        }
    }
});

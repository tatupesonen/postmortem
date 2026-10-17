#![no_main]
//! Carving arbitrary bytes must never panic or hang, and carved ranges
//! must stay inside the source without overlapping.

use libfuzzer_sys::fuzz_target;
use pm_carve::{Options, carve};
use pm_core::io::MemSource;

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else { return };
    let size = rest.len() as u64;
    let src = MemSource::new(rest.to_vec());
    let opts = Options {
        keep_corrupted: flags & 1 != 0,
        chunk: if flags & 2 != 0 { 4096 } else { 1 << 20 },
        ..Options::default()
    };
    let mut end = 0u64;
    carve(&src, &opts, &|| false, &mut |c| {
        assert!(c.len > 0 && c.offset >= end && c.offset + c.len <= size);
        end = c.offset + c.len;
    })
    .unwrap();
});

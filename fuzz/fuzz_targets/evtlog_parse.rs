#![no_main]
//! Event logs: legacy EVT scanning, EVTX parsing and event-map application.

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pm_core::SharedSource;
use pm_core::io::MemSource;

fuzz_target!(|data: &[u8]| {
    let src: SharedSource = Arc::new(MemSource::new(data.to_vec()));
    let maps = pm_evtlog::MapSet::builtin();
    let _ = pm_evtlog::parse(&src, &mut |rec| {
        let _ = maps.apply(&rec);
        true
    });
});

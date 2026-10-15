#![no_main]
//! Opening a hive (with itself as a transaction log), walking every key and
//! value and scanning for deleted keys must never panic or hang.

use std::ops::ControlFlow;
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pm_core::SharedSource;
use pm_core::io::MemSource;
use pm_registry::Hive;

fuzz_target!(|data: &[u8]| {
    let src: SharedSource = Arc::new(MemSource::new(data.to_vec()));
    let Ok(hive) = Hive::open_with_logs(src.clone(), &[src]) else { return };
    hive.walk(100_000, |k| {
        let _ = (k.name(), k.path(), k.class_name(), k.security_descriptor());
        for v in k.values().unwrap_or_default() {
            let _ = (v.name(), v.string(), v.qword(), v.multi_string());
        }
        ControlFlow::Continue(())
    });
    let _ = hive.key("Software\\Microsoft\\Windows NT\\CurrentVersion");
    let _ = hive.deleted_keys(10_000);
});

#![no_main]
//! HTML and XML text extraction from arbitrary bytes: never panics.

use libfuzzer_sys::fuzz_target;
use pm_text::{ExtractOptions, Kind, StringSink, extract};

fuzz_target!(|data: &[u8]| {
    let src = pm_core::io::MemSource::new(data.to_vec());
    for kind in [Kind::Html, Kind::Xml, Kind::Plain] {
        let mut sink = StringSink::with_limit(1 << 20);
        let _ = extract(kind, &src, &mut sink, &ExtractOptions::default());
    }
});

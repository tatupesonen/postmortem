#![no_main]
//! Office Open XML, OpenDocument and EPUB text extraction from arbitrary
//! bytes (ZIP + XML): never panics, bounded output.

use libfuzzer_sys::fuzz_target;
use pm_text::{ExtractOptions, Kind, StringSink, extract};

fuzz_target!(|data: &[u8]| {
    let src = pm_core::io::MemSource::new(data.to_vec());
    for kind in [Kind::Ooxml, Kind::Odf, Kind::Epub] {
        let mut sink = StringSink::with_limit(1 << 20);
        let _ = extract(kind, &src, &mut sink, &ExtractOptions::default());
    }
});

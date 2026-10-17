#![no_main]
//! PDF text and Info-dictionary extraction from arbitrary bytes. Panics
//! inside the PDF library are caught by `pm_text::extract`; this target
//! finds the ones that escape and hangs.

use libfuzzer_sys::fuzz_target;
use pm_text::{ExtractOptions, Kind, StringSink, extract};

fuzz_target!(|data: &[u8]| {
    let src = pm_core::io::MemSource::new(data.to_vec());
    let mut sink = StringSink::with_limit(1 << 20);
    let _ = extract(Kind::Pdf, &src, &mut sink, &ExtractOptions::default());
});

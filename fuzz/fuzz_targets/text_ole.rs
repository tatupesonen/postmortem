#![no_main]
//! OLE2 text extraction (Word 97+, Excel BIFF, PowerPoint, Outlook .msg,
//! summary properties) from arbitrary bytes: never panics.

use libfuzzer_sys::fuzz_target;
use pm_text::{ExtractOptions, Kind, StringSink, extract};

fuzz_target!(|data: &[u8]| {
    let src = pm_core::io::MemSource::new(data.to_vec());
    let mut sink = StringSink::with_limit(1 << 20);
    let _ = extract(Kind::Ole, &src, &mut sink, &ExtractOptions::default());
});

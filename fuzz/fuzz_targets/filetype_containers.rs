#![no_main]
//! Container detectors: the first byte picks a signature (ZIP, OLE2, Ogg,
//! binary plist, gzip, XML) that is prepended to the rest, so the fuzzer
//! spends its time inside the container parsers.

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;

const PREFIXES: [&[u8]; 6] = [
    b"PK\x03\x04",
    b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1",
    b"OggS",
    b"bplist00",
    b"\x1F\x8B",
    b"<?xml version=\"1.0\"?>",
];

fuzz_target!(|data: &[u8]| {
    let Some((&k, rest)) = data.split_first() else {
        return;
    };
    let mut bytes = PREFIXES[usize::from(k) % PREFIXES.len()].to_vec();
    bytes.extend_from_slice(rest);
    let src = MemSource::new(bytes);
    let _ = pm_filetype::tika_detect(&src, None);
    let _ = pm_filetype::has_encrypted_entry(&src);
});

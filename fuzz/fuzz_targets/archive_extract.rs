#![no_main]
//! Extracting arbitrary bytes as any container format must never panic,
//! hang or exceed the configured limits. The first byte selects the format.

use libfuzzer_sys::fuzz_target;
use pm_archive::{Entry, Format, MemberEnd, Options, Sink};
use pm_core::io::MemSource;

const FORMATS: [Format; 16] = [
    Format::Zip,
    Format::SevenZip,
    Format::Rar,
    Format::Tar,
    Format::Gzip,
    Format::Bzip2,
    Format::Xz,
    Format::Zstd,
    Format::Iso9660,
    Format::Docx,
    Format::Xlsx,
    Format::Pptx,
    Format::Doc,
    Format::Xls,
    Format::Ppt,
    Format::Pdf,
];

/// Counts bytes without keeping them.
#[derive(Default)]
struct Count(u64);

impl Sink for Count {
    fn begin(&mut self, _: &Entry) -> std::io::Result<bool> {
        Ok(true)
    }
    fn write(&mut self, d: &[u8]) -> std::io::Result<()> {
        self.0 += d.len() as u64;
        Ok(())
    }
    fn end(&mut self, _: &MemberEnd) -> std::io::Result<()> {
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let format = FORMATS[usize::from(sel) % FORMATS.len()];
    let opts = Options {
        ratio_min_size: 1 << 20,
        max_dictionary: 1 << 24,
        max_in_memory: 1 << 22,
        max_members: 10_000,
        ..Options::default()
    };
    let src = MemSource::new(rest.to_vec());
    let mut sink = Count::default();
    if let Ok(s) = pm_archive::extract(format, &src, &opts, &mut sink) {
        assert!(s.members <= opts.max_members);
        assert_eq!(s.decoded_bytes, sink.0);
    }
});

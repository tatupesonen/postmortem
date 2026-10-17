#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    reason = "benchmark code"
)]
//! Throughput benchmarks, `#[ignore]`d; run with
//! `CARGO_BUILD_JOBS=2 cargo test -p pm-text --release --test bench -- --ignored --nocapture`.
//! Results go to `docs/PERF.md`.

use std::time::Instant;

use pm_text::strings::{Script, StringsConfig, extract};
use pm_text::{ChunkingSink, StringSink, TextSink};

const MB: usize = 1024 * 1024;

/// Deterministic bytes: mostly binary with printable runs.
fn binary(len: usize) -> Vec<u8> {
    let mut x = 0x1234_5678_9ABC_DEF0u64;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if x.is_multiple_of(5) {
            v.extend_from_slice(b"some readable text, mail@example.com ");
        } else {
            v.extend_from_slice(&x.to_le_bytes());
        }
    }
    v.truncate(len);
    v
}

fn report(label: &str, bytes: usize, t: Instant) {
    let s = t.elapsed().as_secs_f64();
    eprintln!("{label}: {:.1} MB/s", bytes as f64 / MB as f64 / s);
}

#[test]
#[ignore = "benchmark"]
fn chunker_throughput() {
    let text =
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Äöü ß 中文 ".repeat(1_000_000);
    let t = Instant::now();
    let mut chunks = 0usize;
    let mut sink = ChunkingSink::new(|_c| {
        chunks += 1;
        Ok(())
    });
    let mut i = 0;
    while i < text.len() {
        let mut j = (i + 64 * 1024).min(text.len());
        while !text.is_char_boundary(j) {
            j -= 1;
        }
        sink.push(&text[i..j]).unwrap();
        i = j;
    }
    sink.finish().unwrap();
    report("chunker (NFKC, lower-casing, sizes)", text.len(), t);
    eprintln!("  {chunks} chunks");
}

#[test]
#[ignore = "benchmark"]
fn strings_throughput() {
    let data = binary(64 * MB);
    let src = pm_core::io::MemSource::new(data.clone());
    let t = Instant::now();
    let mut out = StringSink::default();
    extract(&src, &StringsConfig::default(), &mut out).unwrap();
    report("strings, English-only (LATIN_1)", data.len(), t);
    let intl = StringsConfig {
        utf8: true,
        utf16: true,
        scripts: vec![Script::Latin2, Script::Cyrillic],
    };
    let t = Instant::now();
    let mut out = StringSink::default();
    extract(&src, &intl, &mut out).unwrap();
    report("strings, international (LATIN_2 + CYRILLIC)", data.len(), t);
}

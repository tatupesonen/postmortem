# Performance record (ADR-014 budgets)

Measured on the development machine (Apple Silicon laptop, SSD), release
builds. Re-measure when touching the paths below. Each benchmark is an
`#[ignore]` test or a bench target; the command is listed.

| Path | Budget | Measured | Command | Date |
|------|--------|----------|---------|------|
| Case DB bulk file insert (deferred indexes) | ≥ 200 k files/s | 671 k files/s (1 M rows in 1.49 s) | `cargo test -p pm-casedb --release --test casedb -- --ignored bulk_load_rate --nocapture` | 2026-10-14 |
| Rebuild 9 `files` indexes after load | — | 2.19 s per 1 M rows | same | 2026-10-14 |
| Indexed count (deleted files, 1 M rows) | < 50 ms | 2.4 ms | same | 2026-10-14 |
| Add data source, synthetic 1 M-name tree (walk + writer + index rebuild) | ≥ 150 k rows/s | 205 k rows/s (1 M rows in 4.88 s, 211 MB DB) | `cargo test -p pm-case --release --test case -- --ignored load_rate --nocapture` | 2026-10-14 |
| Children of a 911-entry directory after load | < 20 ms | 1.7 ms | same | 2026-10-14 |
| Extension counts over 1 M rows | < 100 ms | 46 ms | same | 2026-10-14 |
| Carving scan, 512 MiB with 2048 planted files (in memory / file in page cache) | ≥ disk speed | 4 827 / 3 318 MiB/s (Linux, 8 cores, one thread) | `cargo test -p pm-carve --release --test bench -- --ignored --nocapture` | 2026-10-17 |
| Zip extraction, one 256 MiB deflate member / stored member | — | 475 / 11 399 MiB/s decoded (Linux) | `cargo test -p pm-archive --release --test bench -- --ignored --nocapture` | 2026-10-17 |
| Zip extraction, 20 000 small members | — | 406 k members/s (Linux) | same | 2026-10-17 |
| gzip, 256 MiB decoded | — | 377 MiB/s (Linux) | same | 2026-10-17 |
| File type detection (`FileTypeDetector::detect`, mixed synthetic corpus: text, HTML, XML, PDF, PNG, JPEG, EXE, noise, OOXML/ODF/JAR/DOC/XLS) | — | 25.2 k files/s/thread, 177 MB/s (Linux VM, 8 vCPU) | `cargo test -p pm-filetype --release --test detect -- --ignored bench --nocapture` | 2026-10-17 |
| File-name index build, 10 M synthetic names (name + path positional trigrams) | — | 195 k names/s (51 s), 1.59 GB index (Linux, 8 cores shared with 2 other agents' builds) | `cargo test -p pm-search --release --test bench -- --ignored name_index_scale --nocapture` | 2026-10-17 |
| Name search over 10 M names, first page of 100 (rare substring / prefix / exact name / path substring / 2 chars) | < 1 s | 34 / 46 / 27 / 206 / 34 ms | same | 2026-10-17 |
| Name search, 700 k-match substring `secret_`: first page / next page / count | < 1 s | 225 / 152 / 102 ms | same | 2026-10-17 |
| Name search, extension + not deleted + size filter (651 k matches): first page / count | < 1 s | 102 / 66 ms | same | 2026-10-17 |
| Content index add (31 KB chunks, words + trigrams + stored text), 256 MB | — | 14.9 MB/s, index 1.7× text | `cargo test -p pm-search --release --test bench -- --ignored content_index_throughput --nocapture` | 2026-10-17 |
| Ad-hoc search, 8 456 chunks: exact common / rare word, substring, regex (first 100 objects, verified) | < 1 s | 426 / 39 / 4.5 / 5.4 ms | same | 2026-10-17 |
| Inline matcher (5 built-in lists + 101 keywords), one thread | — | 38.9 MB/s | `cargo test -p pm-search --release --test bench -- --ignored matcher_throughput --nocapture` | 2026-10-17 |
| Chunker (NFKC, lower-casing, Autopsy size rules), one thread | — | 32.5 MB/s (follow-up: byte-buffer queue instead of `VecDeque<char>`) | `cargo test -p pm-text --release --test bench -- --ignored --nocapture` | 2026-10-17 |
| String extraction, 64 MiB binary: English-only / international (LATIN_2 + CYRILLIC) | — | 346 / 31 MB/s | same | 2026-10-17 |

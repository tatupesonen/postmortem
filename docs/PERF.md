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
| Re-check of add data source on the Linux VM (8 vCPU, two agent builds running), current master vs `c548a3a` (2026-10-14) | ≥ 150 k rows/s | 89–90 k vs 92 k rows/s: no code regression. The Mac → VM difference is the machine. Bulk insert 246 k files/s, index rebuild 5.05 s per 1 M rows on the VM | `cargo test -p pm-case --release --test case -- --ignored load_rate --nocapture` | 2026-10-17 |
| Carving scan, 512 MiB with 2048 planted files (in memory / file in page cache) | ≥ disk speed | 4 827 / 3 318 MiB/s (Linux, 8 cores, one thread) | `cargo test -p pm-carve --release --test bench -- --ignored --nocapture` | 2026-10-17 |
| Zip extraction, one 256 MiB deflate member / stored member | — | 475 / 11 399 MiB/s decoded (Linux) | `cargo test -p pm-archive --release --test bench -- --ignored --nocapture` | 2026-10-17 |
| Zip extraction, 20 000 small members | — | 406 k members/s (Linux) | same | 2026-10-17 |
| gzip, 256 MiB decoded | — | 377 MiB/s (Linux) | same | 2026-10-17 |
| File type detection (`FileTypeDetector::detect`, mixed synthetic corpus: text, HTML, XML, PDF, PNG, JPEG, EXE, noise, OOXML/ODF/JAR/DOC/XLS) | — | 25.2 k files/s/thread, 177 MB/s (Linux VM, 8 vCPU) | `cargo test -p pm-filetype --release --test detect -- --ignored bench --nocapture` | 2026-10-17 |
| Byte entropy (Encryption Detection, 256 MiB in memory) | — | 3.6 GB/s (Linux VM) | `cargo test -p pm-encryption --release --test bench -- --ignored --nocapture` | 2026-10-17 |
| Same corpus through Apache Tika 3.0.0 (`tika-app`, Autopsy's `FileTypeDetector` call pattern: detect, then detect again with the name; JIT warmed up) | — | 1 177 files/s/thread, 8.3 MB/s (stream, Autopsy's path); 1 292 files/s, 9.1 MB/s (byte[]). pm-filetype on the same run: 27.6 k files/s, 194 MB/s, about 23× faster | corpus via `PM_FILETYPE_CORPUS_DIR=… cargo test -p pm-filetype --release --test detect -- --ignored bench`; Java harness in the PERF notes below | 2026-10-17 |
| File-name index build, 10 M synthetic names (name + path positional trigrams) | — | 195 k names/s (51 s), 1.59 GB index (Linux, 8 cores shared with 2 other agents' builds) | `cargo test -p pm-search --release --test bench -- --ignored name_index_scale --nocapture` | 2026-10-17 |
| Name search over 10 M names, first page of 100 (rare substring / prefix / exact name / path substring / 2 chars) | < 1 s | 34 / 46 / 27 / 206 / 34 ms | same | 2026-10-17 |
| Name search, 700 k-match substring `secret_`: first page / next page / count | < 1 s | 225 / 152 / 102 ms | same | 2026-10-17 |
| Name search, extension + not deleted + size filter (651 k matches): first page / count | < 1 s | 102 / 66 ms | same | 2026-10-17 |
| Content index add (31 KB chunks, words + trigrams + stored text), 256 MB | — | 14.9 MB/s, index 1.7× text | `cargo test -p pm-search --release --test bench -- --ignored content_index_throughput --nocapture` | 2026-10-17 |
| Ad-hoc search, 8 456 chunks: exact common / rare word, substring, regex (first 100 objects, verified) | < 1 s | 426 / 39 / 4.5 / 5.4 ms | same | 2026-10-17 |
| Inline matcher (5 built-in lists + 101 keywords), one thread | — | 38.9 MB/s | `cargo test -p pm-search --release --test bench -- --ignored matcher_throughput --nocapture` | 2026-10-17 |
| Chunker (NFKC, lower-casing, Autopsy size rules), one thread | — | 132 MB/s (was 32.5 before the NFKC quick check and byte-buffer queue) | `cargo test -p pm-text --release --test bench -- --ignored --nocapture` | 2026-10-17 |
| String extraction, 64 MiB binary: English-only / international (LATIN_2 + CYRILLIC) | — | 346 / 31 MB/s | same | 2026-10-17 |
| Virtual disk sequential read, 1 GiB disk (¾ random, ¼ text), 1 MiB requests, warm page cache, content hashed and checked against the raw file: raw / VHD dynamic / VHD fixed / VHDX dynamic / VMDK monolithicSparse / VMDK 2 GB split sparse / VMDK flat / VMDK streamOptimized / QCOW2 / QCOW2 deflate-compressed / QCOW2 zstd-compressed | ≥ disk speed; memory independent of image size | 8 821 / 7 942 / 7 815 / 9 280 / 7 676 / 7 570 / 7 906 / 3 375 / 3 054 / 3 081 / 2 539 MiB/s; RSS growth ≤ 4.5 MiB (bounded table and grain caches) (Linux VM, 8 vCPU, one thread) | `cargo test -p pm-image --release --test vm_bench -- --ignored --nocapture` (needs `qemu-img`) | 2026-10-17 |

## Tika comparison harness

`TikaBench.java` (run with `java -cp tika-app-3.0.0.jar TikaBench.java <corpus dir>`)
loads the corpus into memory and, per file, calls `Tika.detect(TikaInputStream)` and,
unless the result is `application/octet-stream`, `Tika.detect(stream, name)` again,
exactly as Autopsy's `FileTypeDetector.getMIMEType` does. 300 warm-up rounds precede
the 300 timed rounds. Autopsy's own signature table (checked before Tika, a cheap
prefix match) is left out, so the harness times Tika alone. Both detectors gave all
14 corpus files the same MIME type except `data.xml`. Tika says `application/xml`
and pm-filetype says `text/xml`, which is what Autopsy reports because its own
`text/xml` signature wins over Tika.

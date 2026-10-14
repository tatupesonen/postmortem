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

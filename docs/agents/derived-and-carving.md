# Track: derived-and-carving

> **Historical spec** from the parallel-agent period (2026-10-17). The work it describes is merged or folded into other crates; current status is in `docs/PARITY.md`, current direction in `docs/DIRECTION.md`.

Derived files + ingest re-injection (PLAN M4), Embedded File Extractor
(M4), PhotoRec-like carving (M10). Status as of 2026-10-17 (Linux agent,
branch `worktree-agent-ac9d7b3293b4d4267`).

## Done (committed)

### Derived-file infrastructure (ADR-016, earlier session)
* No schema change: derived files are `files` rows with origin
  derived/carved/local, `derived_from`, relative `local_path` (or `layout`
  for carved byte ranges). Shared id allocator `CaseDb::file_ids()`.
* `pm-casedb/src/derived.rs`: `NewFile::new`, `add_derived_files`,
  `derived_files_of` (keyset), `has_children`, `count_children` (new),
  `child_named`, `update_derived_file`.
* XOR1 storage (`pm-case/src/derived.rs`): Autopsy's format, `EncodedWriter`,
  `EncodedSource`, `OutputDir`, `resolve`.
* `pm-ingest`: `JobContext::{reserve_file_ids, add_derived_files,
  insert_derived_files (new: insert without scheduling), add_files_to_job}`;
  re-injected files go to the front of a high-priority queue.

### `pm-archive` (new crate)
Streaming container readers behind one `extract(format, src, opts, sink)`
/ `Sink` API (metadata first, data in 64 KiB chunks):
* zip (in-house: central directory incl. ZIP64 and SFX offsets, local
  header walk for damaged/carved archives, data descriptors; stored,
  deflate, bzip2, LZMA, zstd, xz; ZipCrypto decrypted with a password,
  WinZip AES reported encrypted; NTFS/UT/Unicode-path extras);
* 7z (`sevenz-rust2` 0.23, AES blocks/headers reported encrypted,
  dictionary pre-check); RAR 1.3–7 (`rars` 0.9.4, MIT/Apache, spooled to
  a temp file above the in-memory limit, streamed through a bounded
  channel); tar (ustar, GNU long names / base-256, pax, resync after
  damage); gzip/bzip2/xz/zstd (multi-member/frame); ISO 9660 + Joliet +
  Rock Ridge; OOXML media/embeddings; DOC/XLS/PPT pictures (BLIP records
  over CFB, Excel drawing-group reassembly); PDF attachments + images.
* `detect` / `detect_head` (signatures, zip → OOXML and OLE → doc/xls/ppt
  refinement, ISO probe), `format_for_mime` (Tika aliases).
* Charset repair (`names::repair`, chardetng; CP437 default).
* Zip-bomb rules: Autopsy's declared ratio (≥ 600 for items ≥ 500 MB),
  plus the same ratio on bytes actually decoded per member and per
  container, member cap (500 000), dictionary/window cap (256 MiB),
  in-memory cap (64 MiB). Nesting depth is the module's job.

### Embedded File Extractor (`pm-modules/src/embedded/`)
* Dispatch: `IngestFile::mime()` → `format_for_mime`, else signature
  detection (head bytes; content read only for zip/OLE refinement and,
  when enabled, ISO).
* Archives: tree output under `ModuleOutput/EFE/<id>/<index/1000>/<index>`
  (XOR1), Autopsy's default member names (`x.gz` → `x`, `x.tgz` →
  `x.tar`, `/<archive>/<index>`), raw-name splitting that is safe for
  Shift-JIS, per-directory charset repair, archive timestamps, rows with
  reserved ids inserted parents-first in batches of 1000 through the job
  writer, then scheduled.
* Depth map (per job, entries dropped once used): depth 4 flags the root
  archive; ratio bombs flag the root; `TSK_INTERESTING_ITEM` (score
  likely notable, configuration and `TSK_SET_NAME` "Possible Zip Bomb",
  `TSK_DESCRIPTION` "Zip Bomb Detected <name>", `TSK_COMMENT` with
  Autopsy's detail texts) + warning "Possible ZIP bomb detected in
  archive: …, item: …"; archives below a flagged root are skipped.
  Declared-ratio check skipped for deleted archives and
  `application/x-gzip`, as in Autopsy.
* Encryption: encrypted members listed without content (Autopsy adds
  them with `isFile` false; same here), `TSK_ENCRYPTION_DETECTED` (score
  notable, justification + `TSK_COMMENT` "Password protection detected."
  or "Content-only Encryption (Archive File)") + warning "Encrypted files
  in archive detected.". Error message "Error unpacking <name>" for
  allocated archives.
* Documents: flat children named like Autopsy (OOXML part names, legacy
  `image_<n>.<ext>` with POI's extensions, PDF resource names stored as
  `extract_<n>.<ext>`).
* Re-extraction: existing children (same parent path + name) are kept
  unless the new copy is larger (`update_derived_file`, re-scheduled).
* Settings: `extract_disk_images` (default false).

### Reusable derived-file API (`pm_modules::derive`)
`DerivedTree::new(case_dir, module_folder, &source)`, `child`/`path`,
`create` (XOR1 writer) / `write_file(node, local_name, reader, times)`,
`set`, `commit(ctx, &Existing, on_id)`; `Existing::load`, `split_path`,
`escape_file_name` (Autopsy `FileUtil.escapeFileName`).
For the e-mail track: `add_children(ctx, &parent, "EmailParser",
streams)` stores a flat list of `Stream { name, times, data: &mut dyn
Read }` as derived children (duplicate names kept as separate files,
local copies `ModuleOutput/<folder>/<parent id>/<n>`), schedules them,
returns each stream's file id, and is a no-op on re-ingest. Tested in
`pm-modules/tests/derive.rs`.

### `pm-carve` (new crate) and PhotoRec Carver (`pm-modules/src/photorec.rs`)
* One pass over the source in 4 MiB aligned chunks; headers checked at
  every 512-byte block start through a first-byte dispatch table (as
  PhotoRec does; files start at cluster boundaries); a hit runs the
  format's validator/length finder over a cached window; the scan resumes
  after the carved file.
* Families (PhotoRec `fileopt` names): jpg (marker walk + entropy scan),
  png (chunks), gif (blocks), bmp, tif (IFD extents), pdf (last `%%EOF`
  not followed by an incremental update), zip (EOCD pointing back at the
  start; refined to docx/xlsx/pptx/odt/ods/odp/odg/jar/apk), doc (OLE FAT
  extent; doc/xls/ppt), 7z (start header CRC), gz (inflate to the trailer,
  CRC + size checked), mp3 (ID3v2 + frame walk; ≥ 8 frames when bare),
  riff (avi incl. AVIX, wav, webp, …), mov (box walk; mp4/mov/m4a/m4v/
  3gp/heic/avif/cr3), sqlite (in-header page count), exe/dll (sections +
  security directory), evtx (chunks).
* `photorec_name` (`f%07u.ext`, sector of the unallocated file),
  `map_to_image` (Autopsy `convertToImgRanges`), `KNOWN_EXTENSIONS`
  (Autopsy's list of 333 PhotoRec names).
* Module: unallocated-space files only; Autopsy's settings
  (`keep_corrupted_files`, `extension_filter` NoFilter/Include/Exclude,
  `extensions`) and start-up errors (empty include list, invalid names,
  filter ignoring unallocated space); carved rows (origin carved, unalloc
  name/meta flags, layout = image ranges, `derived_from` = the unallocated
  file) under `/$CarvedFiles/<n>/` (2000 per folder, Autopsy's rule, existing
  folders reused), scheduled into the job; "PhotoRec Results" info
  message.

### Tests (all in the default run)
* `pm-archive`: 25 unit, 25 per-format integration (`tests/formats.rs`),
  4 proptest/seed (`tests/never_panics.rs`), `#[ignore]` bench.
* `pm-carve`: 6 unit, 7 integration (`tests/formats.rs`: every format at
  its exact length with no false positives, chunk-size invariance,
  filters, keep-corrupted, nested stored zip, PDF incremental update,
  cancellation), 2 proptests, `#[ignore]` bench.
* `pm-modules`: `tests/embedded.rs` (6: Autopsy `EmbeddedFileTest`
  cases — nested paths, zip named `.txt`, 1000-file folder, 25-deep
  folders, tgz/7z/RAR, hashes; zip bombs (depth, ratio, deleted archive
  not ratio-checked) and encryption artifacts/messages; documents;
  Shift-JIS repair; ISO opt-in; re-ingest without duplicates and the
  larger-copy update), `tests/photorec.rs` (3: carving into
  `$CarvedFiles`, layout, hashing and EFE of carved files, re-ingest;
  extension filter; start-up refusal), unit tests for texts/settings.
* `pm-ingest/tests/derived.rs`: +1 (`insert_derived_files`).
* Fuzz targets: `archive_extract`, `archive_detect`, `carve_scan`.
  Smoke runs (`cargo +nightly fuzz run <t> -- -max_total_time=…`,
  2026-10-17): `archive_extract` 8 min (≈1.5 M execs after the fix),
  `carve_scan` 8 min (≈4.2 M execs after the fix), `archive_detect` 2 min
  (2.9 M execs). Two crashes found and fixed, inputs kept under
  `fuzz/regressions/{archive_extract,carve_scan}/` and replayed by
  `cargo test` (`fuzz_regressions` in each crate's `never_panics.rs`):
  a subtraction overflow in the zip local-header walk on a truncated
  header, and an addition overflow on a hostile ZIP64 locator in the zip
  length finder.
* `pm-modules/tests/embedded.rs` also runs the real pipeline order (Hash
  Lookup → File Type Identification → EFE) so dispatch on Tika MIME types
  is covered.

### Benchmarks (`docs/PERF.md`)
Carving 512 MiB: 4.8 GiB/s in memory, 3.3 GiB/s from a file in the page
cache (I/O bound in practice). Zip deflate 475 MiB/s, stored 11 GiB/s,
20 000 small members at 406 k/s; gzip 377 MiB/s.

## Known gaps vs Autopsy
* Derived-file tool name/version () is not
  stored (no schema change); the producer is implied by the
  `ModuleOutput/<module>` folder.
* "Unzip contents with password" action is UI (coordinator-owned);
  `pm_archive::Options::password` and the module are ready for it.
* Carving covers 17 PhotoRec families, not PhotoRec's ~300; no external
  `photorec` runner. Fragmented files are carved as contiguous (as
  PhotoRec does without its brute-force mode).
* Autopsy's free-disk-space check (dead code there) and FileTaskExecutor
  retries for network shares are not ported.
* Documents: no OOXML extension-from-MIME for nameless parts (pm-archive
  always has part names); PDF inline-image dedup follows pm-archive.
* Deliberate differences (documented in the module docs): re-ingest
  schedules only new/updated members and never duplicates document or
  carved children; header-encrypted 7z/RAR get the encryption result;
  members completed before a streaming bomb stop are kept; xz/zstd and
  opt-in ISO extraction.

## Notes for the coordinator
* **Gate flake (pm-ui, not this track)**: after merging master d627a89
  the gate failed 3 times in a row, each time with exactly one failure in
  `pm-ui/tests/ui_flows.rs` and a different test each run
  (`hosts_and_persons` assertion at line 945 twice — its context menu
  closes between `settle` and the final query; `tagging_files_feeds_tags_and_score_nodes`
  / `deleted_files_and_metadata_tab` "UI did not settle" under load).
  Each passes in isolation and the whole binary passes with
  `--test-threads=1` when the machine is idle; all other 654 tests pass.
  The last commit of this track (fuzz fixes, helper API, pipeline-order
  test) was made after these runs; please re-run the gate when merging.
* `core_modules()` now includes the Embedded File Extractor (after Hash
  Lookup; File Type Identification belongs before it) and the PhotoRec
  Carver (enabled by default, as in Autopsy; it refuses to start when the
  file filter ignores unallocated space).
* `fuzz/Cargo.toml` had a broken `[[bin]]` entry (`evtlog_parse` and
  `registry_hive` merged); fixed while adding the archive/carve targets.
* UI follow-ups: password prompt for encrypted archives; module settings
  panels for EFE (`extract_disk_images`) and PhotoRec (filter list,
  keep corrupted).

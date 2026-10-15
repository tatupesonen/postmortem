# Track: derived-and-carving

Derived files + ingest re-injection (PLAN M4), Embedded File Extractor
(M4), PhotoRec-like carving (M10). Status as of the coordinator's pause
point, 2026-10-15.

## Done (committed)

### Derived-file infrastructure (ADR-016)
* **No schema change** (per coordinator): derived files are `files` rows
  with `origin` derived/carved/local, `derived_from`, relative
  `local_path` (or `layout` for carved byte ranges).
* **Shared file-id allocator** `CaseDb::file_ids()` (`FileIds`: atomic,
  initialised from `MAX(id)+1` on open; `next`, `reserve(n)`, `peek`).
  The loader (`pm-case/src/load.rs`) and `Bulk::add_file` use it; rows
  without an explicit id never let SQLite choose one, and explicit ids
  advance the allocator.
* `pm-casedb/src/derived.rs`: `NewFile::new(..)` (row with defaults),
  `NewFile::to_record(id)`, `CaseDb::add_derived_files`,
  `derived_files_of` (keyset), `has_children`, `child_named`,
  `update_derived_file` (Autopsy re-extraction update: size, times,
  local path, optional MIME reset, hashes cleared).
* **XOR1 storage** (`pm-case/src/derived.rs`): TSK's exact format (32-byte
  plain header `TSK_CONTAINER_XOR1_xxxxxxxxxxxxx`, bytes XOR 0xCA),
  `EncodedWriter` (buffered), `EncodedSource` (decoding `ReadAt`),
  `sniff_encoding`, `OutputDir` (`ModuleOutput/<module>/<source id>/…`)
  and `resolve` (stored paths cannot escape the case folder; `\` paths
  from Windows-written cases accepted).
* `Case::content` for derived/local files decodes XOR1 (detected by header,
  as TSK does); local files with absolute paths are read unchanged.
* **pm-ingest re-injection** (`sched.rs`, `context.rs`, `job.rs`):
  `JobContext::{reserve_file_ids, add_derived_files, add_files_to_job}`.
  Derived rows go through the job's single writer, which commits and
  acknowledges, then they are pushed to the *front* of a high-priority
  queue (Autopsy `putFirst`). Termination: a worker stops only when both
  queues are empty, no task runs and no hold (producer, stage-1
  data-source modules) is open. The producer streams only ids below the
  allocator value at job start (no double processing). Files added by a
  later data-source stage get an extra file phase (Autopsy drops them).
  The file filter applies to added files; nothing is scheduled after
  cancellation.

### Tests
* `pm-casedb/tests/derived.rs` (7): round trip, update, data-source
  deletion, concurrent allocation (8 threads), explicit ids, reopen,
  defaults.
* `pm-case` unit tests (3): XOR1 byte layout vs TSK, plain pass-through,
  path confinement.
* `pm-ingest` scheduler unit tests (4) and `tests/derived.rs` (5):
  recursive re-injection exactly once (depth 4, 2 roots, 18 files) with
  XOR1 content and artifacts on derived files; cancellation; stage-1
  (delayed) and stage-2 data-source modules adding files; re-ingest
  processes existing derived files once; filter applied to added files.

## In progress (uncommitted in this worktree's scratch copy)

`pm-archive` crate (new): streaming container readers behind one
`Archive`/`Sink` API. Written and compiling, with 21 unit tests passing:
zip (`zip` 8.6), 7z (`sevenz-rust2` 0.23, AES blocks reported encrypted,
dictionary pre-check), in-house bounded tar walker (GNU long names, PAX),
gzip/bzip2/xz/zstd (multi-member/frame), RAR all versions (`rars` 0.9.4,
spooled to a temp file above the in-memory limit, streamed via a bounded
channel), in-house ISO 9660 + Joliet reader, OOXML media/embeddings,
DOC/XLS/PPT pictures (validating BLIP scanner over the CFB streams,
Excel drawing-group reassembly), PDF attachments + images (`lopdf`,
bounded decompression), signature detection with zip/OLE refinement,
filename charset repair (`chardetng`), and the **zip-bomb rules**
(`bomb.rs`): Autopsy's declared per-item ratio (≥ 600 for items
≥ 500 MB) and depth 4, plus the same ratio applied to *actually decoded*
bytes per member and per container while streaming (lying headers,
single-stream formats, overlapping-entry "non-recursive" bombs), member
caps, decoder dictionary/window caps and in-memory caps. Licences of the
whole dependency tree checked (all permissive); too-new transitive
crates pinned to versions ≥ 2 weeks old. A copy is kept in the
session scratchpad (`dc-wip/`); it will be restored on resume.

Blocker found: the test-fixture builders (`pm-testkit/src/archives.rs`)
enable `zip`'s `aes-crypto`, which needs `constant_time_eq ^0.4`, not
resolvable from the index snapshot; drop AES fixtures or pick another
route on resume.

## Remaining (next steps, in order)
1. Fix the fixture builders; pm-archive integration tests per format,
   proptest never-panic, fuzz targets (`fuzz/`: zip, 7z, tar, single,
   rar, iso, ole, pdf, detect), commit.
2. Embedded File Extractor module (`pm-modules/src/embedded/`): MIME
   dispatch (File Type ID MIME or `pm_archive::detect`), XOR1 output
   under `ModuleOutput/EFE/<id>/<index/1000>/<index>`, derived tree with
   reserved ids, charset fix, archive timestamps, depth map + root-archive
   flagging (`TSK_INTERESTING_ITEM`, set "Possible Zip Bomb", Autopsy
   description/comment/messages), streaming bomb meter,
   `TSK_ENCRYPTION_DETECTED` ("Password protection detected." /
   "Content-only Encryption (Archive File)"), re-extraction (existing
   files: skip unless larger), inbox messages; e2e tests on pm-testkit
   NTFS cases (zip inside NTFS, nested, bomb, encrypted, docx/doc/pdf).
3. Carving: `pm-carve` (single-pass, large aligned reads, memchr/
   aho-corasick headers, validators/length finders for jpg, png, gif,
   bmp, pdf, zip/OOXML, OLE, mp3, RIFF avi/wav, mp4/mov, sqlite, exe, …),
   PhotoRec Carver module (unalloc only, settings, `$CarvedFiles`,
   layout mapping through unalloc ranges, re-injection), benchmark in
   `docs/PERF.md`, fuzz target.

## Known gaps vs Autopsy (so far)
* Derived-file tool name/version (`tsk_files_derived_method`) is not
  stored (no schema change); the producing module is implied by the
  `ModuleOutput/<module>` folder.
* "Unzip contents with password" action is UI (coordinator-owned).

## Notes for the coordinator
* `pm-ui` test `views_list_files_by_type_from_the_case_database` is flaky
  under load (failed once in the gate, then 1 of 3 isolated runs): the
  view counts are loaded once and only refreshed while "live", so an
  early count of `Images (0)` sticks. Not caused by this track.
* The session scratchpad is shared between agents: a fixed log name
  (`gate1.log`) was overwritten by another agent's gate output. Use
  unique names.

# Track: core-file-modules (PLAN.md M4)

Autopsy's core file-level ingest modules. Status at the coordinator's pause
point (2026-10-15): **item 1 (File Type Identification) is done**; items
2–5 are not started.

| # | Item | Status |
|---|------|--------|
| 1 | `pm-filetype` + File Type Identification module | done |
| 2 | Extension Mismatch Detector | not started |
| 3 | Encryption Detection | not started (`pm_filetype::cfb` and `zip::has_encrypted_entry` are ready for it) |
| 4 | Interesting Files Identifier + reusable FilesSet engine (`pm-rules`) | not started |
| 5 | Data Source Integrity (+ `pm-casedb/src/integrity.rs`) | not started |

## 1. File Type Identification

### What exists

**`crates/pm-filetype`** (new crate, no C, no `unsafe`):

* `MimeDb` — Apache Tika 3.0.0's `tika-mimetypes.xml`, embedded verbatim
  (`data/`, Apache-2.0, NOTICE in `data/TIKA-NOTICE.txt`, SHA-256 in
  `db.rs`), parsed once at first use (~ms). Ports `MimeTypesReader`,
  `MimeTypes.detect`, `Magic`/`MagicMatch`/`MagicDetector` (priorities,
  offset ranges, masks, `string`/`stringignorecase`/`unicodeLE`/numeric
  types incl. Tika's octal default and the `byte` quirk, zero-padding past
  EOF, the 8 KiB regex window), `And`/`Or`/`MinShouldMatch` clauses, the
  magic sort order (priority, size, type, string), `Patterns` (names,
  extensions longest-first, globs, regex globs, lower-case retry),
  `MediaTypeRegistry` (aliases, sub-class-of, `+xml`/`+zip`/`text/*`
  fallbacks), root-XML refinement with the TIKA-2419 HTML/text downgrade,
  `TextDetector`/`TextStatistics`, and `java.net.URI` resource-name
  handling (opaque ADS names give no hint).
  Java regexes are translated to byte regexes (`.` excludes `\r`/`\x85`);
  the two patterns Rust's `regex` cannot express (dBase look-behind,
  MS-Office owner-file back-reference) are hand-coded.
* Container detectors in Tika's composite order (non-Tika classes first,
  then class name), each replacing the result only with a specialisation:
  vorbis-java-tika `OggDetector` (page/packet walk incl. its 64 KiB junk
  limit and leftover quirk), `BPListDetector` (validating binary-plist
  parser; corrupt plists → Tika throws → octet-stream),
  `GZipSpecializationDetector`, `POIFSContainerDetector` and
  `MiscOLEDetector` over an own OLE2 reader (`cfb.rs`, validates like POI;
  ≥ 128 MiB → generic OLE), `DefaultZipContainerDetector` streaming path
  (Autopsy's stream is not file-backed) with commons-compress archive and
  compressor sniffing and all eight `ZipContainerDetector`s (iWork, OPC with
  POI's content types, IPA, JAR/APK/WAR/EAR, KMZ, ODF `mimetype`,
  StarOffice manifest, Frictionless/WACZ), 16 MiB limit, data-descriptor
  retry.
* `FileTypeDetector` — Autopsy's `getMIMEType`: octet-stream special cases,
  600-byte user-defined then Autopsy-defined signatures (all 22, in order),
  Tika pass 1 without name, pass 2 with the name, `tika-` and parameter
  removal, `.txt` charset fallback, the `audio/mpeg` `FF FF` fix.
  `FileType`/`Signature` (RAW/ASCII, from start/end, all-must-match) with
  serde settings and Autopsy's legacy `UserFileTypeDefinitions.xml` import.
* Reads Tika's 64 KiB magic prefix (reusing `IngestFile::head()` for small
  files); containers read more only when their signature matches.

**`crates/pm-modules/src/file_type.rs`** — module `file_type`
("File Type Identification"), registered right after Hash Lookup. Settings
`{ user_types: [FileType] }` (validated at start-up; an invalid type fails
the job). Sets the MIME type; the first matching user type with
`create_interesting_file_hit` posts `TSK_INTERESTING_ITEM` (Likely Notable,
configuration and `TSK_SET_NAME` = set name, `TSK_CATEGORY` = the type's
MIME), de-duplicated on re-ingest. Summary inbox message "File Type Id
Results". `mime_or_detect()` gives later modules Autopsy's
detect-if-missing behaviour. Picture Analyzer's own JPEG/TIFF/WAV sniffing
fallback is unchanged.

**Decision (directories):** directories and virtual directories get **no**
MIME type (Autopsy writes `application/octet-stream` but hides it); other
non-regular files, empty files, unallocated space and slack < 4096 bytes get
`application/octet-stream` like Autopsy. Resolves the open item in
inventory 08 §3.

### Tests

* `pm-filetype`: 35 unit tests (media types, registry, magic decoding and
  matching incl. padding/regex windows, globs/URI names, XML roots, text
  statistics, OLE/ZIP/bplist/Ogg/archive detectors with generated fixtures,
  detector special cases), `tests/detect.rs` (proptest "never panics" over
  13 container-routing prefixes, detectable-type list, `#[ignore]`
  throughput benchmark).
* `pm-modules/tests/file_type.rs`: 4 end-to-end tests on the demo disk
  (types, directory/unalloc/empty rules, user-type interesting hit +
  idempotent re-ingest, start-up failure, Picture Analyzer after typing).
* Fuzz targets `filetype_detect`, `cfb_open`, `zip_stream`; smoke runs
  (40 s / 30 s / 30 s, ~2.2 M executions) found no crashes.
* **Differential test against real Tika 3.0.0** (not in CI; needs Java):
  `crates/pm-filetype/examples/tika_oracle.rs` and an equivalent Java
  harness (`Tika.detect` on a non-file-backed `TikaInputStream`, both
  passes, Autopsy's clean-up) over a 20,517-file stratified corpus (1,715
  extensions from /System, /Applications, Homebrew, the Autopsy tree and the
  cargo registry): **20,509 identical (99.96 %)**. To reproduce: build
  `tika-app-3.0.0.jar` + the harness (`Tika.detect(TikaInputStream.get(new
  FilterInputStream(new FileInputStream(f)){}))`, then with the name),
  feed both the same path list and diff the TSVs.

### Known gaps vs Autopsy/Tika

* Remaining corpus differences (8 files): Vue files starting with an HTML
  comment (Tika `text/plain`, we `text/html`: SAX root-element error
  semantics differ); two LibreOffice `.sob/.sop` palettes (we report
  StarOffice autotext, Tika `application/zip`: manifest parsing differs); one
  `.appx` (Tika `x-ooxml`, we `zip`); one `.cur` (we match the TGA magic,
  Tika does not — regex window/padding edge).
* `.txt` charset fallback approximates ICU4J's `CharsetDetector` +
  `decodetect` with a BOM/UTF-16/low-control-character heuristic.
* XML root extraction honours UTF-8/UTF-16 (BOM or pattern) only; other
  declared encodings are read as bytes. Java `\b` is ASCII here.
* `.vsdx`-style OPC detection and iWork 13 by-message-type are as in Tika's
  streaming path (Tika's file-backed path would differ; Autopsy uses the
  streaming one).
* Throughput 9.3 k files/s/thread: every text file evaluates all 847
  magics (incl. 36 regexes on a zero-padded 8 KiB window) — a first-byte
  prefilter is the obvious optimisation.

## Follow-ups (when resumed)

Items 2–5 above, in that order, each committed separately; Encryption
Detection reuses `pm_filetype::cfb`, `zip::has_encrypted_entry`,
`text::TextStatistics`, and `file_type::mime_or_detect`.

## Verification status of this commit

* `scripts/gate.sh` fails only in `pm-ui` (pre-existing clippy errors in
  `pm-ui/src/timeline_chart.rs` from the last ungated master commits;
  this track may not edit `pm-ui`). The coordinator then asked to stop
  running gates, so this is a WIP commit.
* Verified instead: `cargo fmt --all --check`; `cargo clippy --workspace
  --exclude pm-ui --exclude postmortem --all-targets --all-features -D
  warnings` clean; `cargo test --workspace --exclude pm-ui --exclude
  postmortem --all-features`: 348 passed, 0 failed.
* Unverified: the `pm-ui`/`postmortem` crates build and tests with these
  changes (they do not call the new code), and a full gate run.
* Also fixed four pre-existing clippy errors in
  `pm-modules/src/recent_activity` (`is_multiple_of`, a doc backtick, a
  type alias, a `too_many_arguments` allow) so `pm-modules` lints clean.

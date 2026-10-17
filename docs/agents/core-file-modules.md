# Track: core-file-modules (PLAN.md M4)

Autopsy's core file-level ingest modules, reimplemented on Linux after the
first implementation was lost (the predecessor's unverified WIP was
merged from `wip/filetype`, reviewed, fixed and completed).

| # | Item | Status |
|---|------|--------|
| 1 | `pm-filetype` + File Type Identification module | done |
| 2 | Extension Mismatch Detector | not started |
| 3 | Encryption Detection | not started |
| 4 | Interesting Files Identifier + reusable FilesSet engine (`pm-rules`) | not started |
| 5 | Data Source Integrity (+ `pm-casedb/src/integrity.rs`) | not started |

Pipeline order in `core_modules()` follows Autopsy's effective order
(`PipelineConfig.xml`, then `IngestModuleFactoryLoader.coreModuleOrdering`
for unlisted core modules): Hash Lookup, File Type Identification,
Picture Analyzer, Extension Mismatch, Interesting Files, Encryption
Detection; Data Source Integrity is a stage-2 data-source module.

## 1. File Type Identification

### `crates/pm-filetype` (no C, no `unsafe`)

* `MimeDb`: Apache Tika 3.0.0's `tika-mimetypes.xml`, embedded verbatim
  (`data/`, Apache-2.0, NOTICE in `data/TIKA-NOTICE.txt`), parsed once on
  first use. Ports `MimeTypesReader`, `MimeTypes.detect`, `MagicMatch` /
  `MagicDetector` (priorities, offset ranges, masks, string / ignore-case /
  unicodeLE / numeric types incl. Tika's octal default and the `byte`
  quirk, zero padding past EOF, the 8 KiB regex window), And / Or /
  MinShouldMatch clauses, the magic sort order, `Patterns` (names,
  extensions longest first, globs, lower-case retry), the
  `MediaTypeRegistry` (aliases, sub-class-of, `+xml`/`+zip`/`text/*`
  fallbacks), root-XML refinement incl. the TIKA-2419 downgrade,
  `TextDetector`, and `java.net.URI` resource-name handling. Java regexes
  are translated to byte regexes; the two Rust's `regex` cannot express
  (dBase look-behind, MS-Office owner back-reference) are hand-coded.
* Container detectors in Tika's composite order, each replacing the
  answer only with a specialisation: `OggDetector`, `BPListDetector`
  (dd-plist 1.28 validation, corrupt → Tika throws → octet-stream),
  `GZipSpecializationDetector`, `POIFSContainerDetector` +
  `MiscOLEDetector` over an own OLE2 reader (`cfb`, POI-style
  validation), `DefaultZipContainerDetector`'s streaming path (Autopsy's
  stream is not file-backed) with Commons Compress archive/compressor
  sniffing and all eight `ZipContainerDetector`s, 16 MiB limit,
  data-descriptor retry.
* `FileTypeDetector`: Autopsy's `getMIMEType` — octet-stream special
  cases, user-defined then Autopsy-defined signatures (all 22, in order)
  on a 600-byte buffer, Tika pass 1 without the name and pass 2 with it,
  `tika-` and parameter removal, the `.txt` charset fallback, the
  `audio/mpeg` `FF FF` fix. `FileType` / `Signature` (RAW/ASCII, from
  start/end, all must match) with serde settings, validation and import
  of Autopsy's legacy `UserFileTypeDefinitions.xml`.
* `ZipBuilder` / `cfb::builder` (feature `builder`) for fixtures.

Fixes to the predecessor's WIP found by the differential run:
* bplist: dd-plist reads an empty integer range as 0 without indexing, so
  a trailer with a 0 offset size is accepted (Tika: `x-bplist`);
* XML: internal DTD entities are now stored and expanded in attribute
  values (RDF/OWL `xmlns:rdf="&rdf;"`), with recursion/size guards;
* ZIP: skipping a deflated data-descriptor entry stopped after the first
  inflate call, so OOXML/XPS files with `[Content_Types].xml` last were
  reported as plain ZIP.

Performance: one zero-padded regex window per detection (was one 8 KiB+
allocation per regex magic per file), a `memchr` first-byte search for
ranged magics, early exit in min-should-match: 7.3 k → 25.2 k files/s
per thread (`docs/PERF.md`).

### `crates/pm-modules/src/file_type.rs`

Module `file_type` ("File Type Identification", "Matches file types based
on binary signatures."), registered right after Hash Lookup. Settings
`{ "user_types": [FileType] }`; an invalid type fails start-up with
Autopsy's "Unable to run file type detection.". Sets the MIME type; the
first matching user type with `create_interesting_file_hit` posts
`TSK_INTERESTING_ITEM` (Likely Notable, configuration and `TSK_SET_NAME`
= set name, `TSK_CATEGORY` = MIME type), de-duplicated on re-ingest. The
job ends with the "File Type Id Results" info message (total processing
time, files processed). `mime_or_detect()` + `default_detector()` give
later modules Autopsy's detect-if-missing behaviour.

**Decision (ADR-019):** directories and virtual directories get no MIME
type; every other object follows Autopsy (octet-stream for empty files,
unallocated blocks, slack < 4096 bytes and non-regular objects).

### Tests

* `pm-filetype`: 35 unit tests (media types, registry, magic decoding and
  matching incl. padding and regex windows, globs and URI names, XML roots
  and entity expansion, text statistics, OLE / ZIP (data descriptors) /
  bplist / Ogg / archive detectors on generated fixtures, Autopsy rules,
  legacy XML import) and `tests/detect.rs` (container samples, detectable
  type lists, proptest "never panics" over 18 routing prefixes and over
  randomly corrupted containers, `#[ignore]` throughput benchmark).
* `pm-modules/tests/file_type.rs`: 4 end-to-end tests (19 typed paths on
  a generated NTFS volume incl. ADS, user types with/without hits,
  re-ingest idempotence, the summary message; start-up failures; demo
  disk rules for unallocated space, FAT virtual files and directories
  with the Picture Analyzer downstream; `mime_or_detect`).
* Fuzz targets `filetype_detect`, `filetype_containers`, `cfb_open`; smoke
  runs of 60 s each (126 k / 264 k / 4.7 M executions): no crashes.
* **Differential test against real Tika 3.0.0** (opt-in, Java):
  `crates/pm-filetype/oracle/run.sh OUT ROOT...`. On 10,861 files (Tika's,
  POI's and Commons Compress' test documents plus a stratified sample of
  /usr, /etc, /opt, /snap, the Autopsy tree, the cargo registry and user
  documents), both Tika passes and Autopsy's combined answer are
  **100 % identical**.

### Known gaps vs Autopsy/Tika

* The `.txt` charset fallback approximates ICU4J's `CharsetDetector`
  with a BOM / UTF-16 / low-control-character heuristic.
* XML roots honour UTF-8/UTF-16 (BOM or pattern) and ASCII; other
  declared 8-bit encodings are read as Latin-1 (names are ASCII anyway).
* Only Tika's streaming ZIP path is ported (Autopsy never gives Tika a
  file-backed stream).

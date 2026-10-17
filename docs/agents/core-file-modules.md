# Track: core-file-modules (PLAN.md M4)

Autopsy's core file-level ingest modules, reimplemented on Linux after the
first implementation was lost (the predecessor's unverified WIP was
merged from `wip/filetype`, reviewed, fixed and completed).

| # | Item | Status |
|---|------|--------|
| 1 | `pm-filetype` + File Type Identification module | done |
| 2 | Extension Mismatch Detector | done |
| 3 | Encryption Detection (+ `pm-encryption`) | done |
| 4 | Interesting Files Identifier + reusable FilesSet engine (`pm-rules`) | not started |
| 5 | Data Source Integrity (+ `pm-casedb/src/integrity.rs`) | not started |

Pipeline order in `core_modules()` follows Autopsy's effective order
(`PipelineConfig.xml`, then `IngestModuleFactoryLoader.coreModuleOrdering`
for unlisted core modules): Hash Lookup, File Type Identification,
Embedded File Extractor, Picture Analyzer, Extension Mismatch, Interesting Files, PhotoRec, Encryption
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

## 2. Extension Mismatch Detector

`crates/pm-modules/src/ext_mismatch.rs`, module `ext_mismatch`
("Extension Mismatch Detector", "Flags files that have a non-standard
extension based on their file type."), after the Picture Analyzer.

* Settings: `skip_files_without_extension` (true), `skip_known_files`
  (true), `check_type` (`all` / `no_text_files` / `only_media_and_exe`,
  default the latter with Autopsy's 28 `MEDIA_AND_EXE_MIME_TYPES`) and
  `mismatch_map` (MIME type → allowed extensions; `null` = Autopsy's
  default `mismatch_config.xml`, embedded verbatim in
  `crates/pm-modules/data/`, 65 types). `parse_config` / `write_config`
  read and write Autopsy's XML (a type without `<ext>` is listed but not
  checked; a later signature replaces an earlier one).
* Skips known files (option), unallocated space and slack, non-regular
  objects, and files whose name or metadata is unallocated. The MIME type
  comes from `mime_or_detect`.
* A mismatch posts `TSK_EXT_MISMATCH_DETECTED` (Likely Notable,
  justification "File has MIME type of <type>", no attributes),
  de-duplicated on re-ingest (Autopsy itself would duplicate it). The job
  ends with "File Extension Mismatch Results" when any file was compared.

Tests: 5 unit tests (default config, XML round trip and errors, an
18-row `compareSigTypeToExt` decision table, settings serde, proptest on
the XML reader) and 3 end-to-end tests (default settings incl. known and
deleted files and the summary, every check mode and skip option, a
custom map).

Gap: none known in behaviour. The settings UI (global map editor, "Add
extension as matching MIME type" context action) is a coordinator to-do.
## 3. Encryption Detection

### `crates/pm-encryption` (new)

Reusable checks, each answering what Autopsy learns from Tika/Jackcess:
`Ok(true)` (an encrypted-document exception), `Ok(false)`, or a parse
error (Autopsy logs it and flags nothing).

* `entropy`: Autopsy's byte entropy, including its `size - 1` divisor;
  streamed in 1 MiB reads with cancellation.
* `volume`: Autopsy's BitLocker boot-sector rule (incl. its signed-byte
  quirk that rejects 0x80 sectors/cluster), plus LUKS 1/2 and Core
  Storage (FileVault 2) headers.
* `office`: legacy Word/Excel/PowerPoint as Apache POI 5.3 opens them:
  the FIB `fEncrypted` flag and table-stream `EncryptionInfo` (Word),
  BOF/WRITEPROTECT/FILEPASS (Excel), CurrentUser → UserEditAtom →
  PersistPtrHolder → DocumentEncryptionAtom (PowerPoint, ranged OLE reads);
  XOR, binary RC4, RC4 CryptoAPI and AES "standard" verifiers tried with
  POI's default password `VelvetSweatshop`; which POI exceptions Tika
  reports as `EncryptedDocumentException` follows each extractor.
* `pdf`: PDFBox 3.0.3's standard security handler with an empty password
  (owner first, then user) for revisions 2–6 (RC4 40/128, AES-128,
  AES-256 with algorithm 2.B); trailer discovery via `startxref`, classic
  xref tables, xref streams (Flate + PNG predictors), `/Prev` chains,
  hybrid `/XRefStm`, and a brute-force fallback like PDFBox's lenient
  parser.
* `access`: Jackcess 4.0.7 with `CryptCodecProvider`: Office-encrypted
  ACCDB → protected; unsupported providers or unopenable files → not;
  otherwise the de-obfuscated database password field (header mask +
  creation-date mask).

Verified against the real libraries: `scripts/fixtures/MakeEncrypted.java`
writes 17 documents with POI/PDFBox (protected and unprotected DOC, XLS
incl. Excel's default password, PPT, OOXML agile/standard, PDF owner-only
and R2/R3/R4/R6) and prints real Tika 3.0.0's verdict for each; plus
POI-written FILEPASS vectors; `AccessOracle.java` runs Autopsy's Jackcess
path on Tika's Access test databases. `tests/oracle.rs` asserts every
verdict (all identical).

### `crates/pm-modules/src/encryption.rs`

Module `encryption` ("Encryption Detection", "Looks for files with the
specified minimum entropy.").

* Settings (Autopsy defaults): `minimum_entropy` 7.5 (6.0–8.0),
  `minimum_file_size` 5 242 880 bytes (≥ 1), `file_size_multiple_enforced`
  true, `slack_files_allowed` true; invalid values fail start-up with
  Autopsy's messages.
* File level: skips unallocated space, directories, slack (unless
  allowed), known files, unallocated metadata and Autopsy's (misspelt)
  `/hiberfile.sys`, `/pagefile.sys`. `application/octet-stream` files of a
  qualifying size (`.db` from 64 KiB) with entropy ≥ minimum →
  `TSK_ENCRYPTION_SUSPECTED` (Likely Notable, "Suspected encryption due to
  high entropy (%f)."); else protected OOXML / DOC / XLS / PPT / PDF /
  Access → `TSK_ENCRYPTION_DETECTED` (Notable, "Password protection
  detected."). Comment as `TSK_COMMENT` and justification; an inbox data
  message "<type> Match: <name>" with "File: <path>" and the entropy.
* Data-source level (stage 1, disk images): per volume, BitLocker →
  DETECTED "Bitlocker encryption detected."; beyond Autopsy, LUKS →
  DETECTED "LUKS encryption detected." and Core Storage → SUSPECTED; a
  volume without a file system whose entropy reaches the minimum →
  SUSPECTED; the first flagged volume ends the check (Autopsy). Volume
  results are attached to the data source with the volume in
  `TSK_DESCRIPTION` (no volume objects in the case database).
* Re-ingest posts no duplicates (Autopsy would).

Tests: 16 unit tests (crypto vectors incl. FIPS-197, entropy, BitLocker/
LUKS/Core Storage rules, POI vectors, persist pointers, Access headers
for Jet3/Jet4/ACE and Office providers, PDF objects/predictors), the
oracle tests, proptests over corrupted fixtures and arbitrary bytes, fuzz
target `encryption_check`; end-to-end: protected and unprotected
documents, VeraCrypt-style containers and `.db` files with the size rules,
settings validation, BitLocker / LUKS / random / zero volumes on
partitioned disks.

Gaps vs Autopsy: Agile-encrypted binary Office files and Office-encrypted
ACCDBs are assumed not to open with the default/empty password (POI and
Jackcess would verify it; such files do not exist in practice); an Access
database with a password but a corrupt catalog is flagged (Jackcess
fails to open it); the PDF check does not parse the document beyond the
trailer and encryption dictionary (PDFBox may reject a broken PDF
first); APFS (FileVault on APFS) volumes are not recognised.


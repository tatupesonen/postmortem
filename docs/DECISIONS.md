# Architecture decision records

## ADR-001 — Pure-Rust evidence parsing
All image, volume, filesystem and artefact parsing is written in Rust (own
code or pure-Rust crates). No libtsk/libewf FFI. Rationale: memory safety on
hostile input, no C toolchain, simpler cross-platform builds. Consequence:
filesystems arrive incrementally and need extensive fixture/fuzz testing.

## ADR-002 — Own case schema, not Autopsy-compatible
postmortem uses its own case-database schema, designed around the queries the
UI and ingest actually run (covering indexes, integer enums, WAL, batched
writes). SQLite for single-user cases and PostgreSQL for multi-user, behind
one storage trait. An Autopsy-case importer can be added later.

## ADR-003 — Positional `ReadAt` as the universal byte-source abstraction
Every evidence layer implements `pm_core::io::ReadAt` (`&self` positional
reads, `Send + Sync`). This allows lock-free sharing between ingest threads
and composes images → volumes → files.

## ADR-004 — Panic-free parsing enforced by lints
`clippy::indexing_slicing` and `unwrap_used` are denied workspace-wide outside
tests. Parsers use `pm_core::Bytes` checked accessors. Where an index is
provably in range and the checked form hurts clarity, use
`#[expect(clippy::indexing_slicing, reason = "...")]`.

## ADR-005 — Metric (SI) units by default
Sizes are shown with SI prefixes (kB, MB, GB) by default (organisation
policy), with IEC available. Timestamps are ISO-8601, UTC by default, with a
configurable display time zone.

## ADR-006 — Local git checkpoints
The autonomous loop commits each finished, green slice to a local git
repository in `postmortem/`. It never pushes or adds remotes.

## ADR-007 — Database layer: rusqlite + postgres, no sqlx/sea-orm
SQLite access goes through `rusqlite` (bundled), PostgreSQL through
`postgres`/`tokio-postgres` (+ `deadpool-postgres`). Dialect-portable SQL is
built with `sea-query` where it helps, hand-written SQL elsewhere, and
migrations use `refinery` (both back ends). `sqlx`, `sea-orm` and
`sea-query-rusqlite` are banned: they pin conflicting `libsqlite3-sys`
versions (`links = "sqlite3"`). Inventory 07 §0.3 and §14.

## ADR-008 — GUI stack: eframe/egui 0.36, egui_dock, deferred viewports
UI is eframe 0.36.2 (wgpu) with `egui_dock` for the docked main layout and
**deferred viewports** for native pop-out windows. Our own code manages the
pop-outs, because egui_dock only tears tabs out *inside* the window.
Workarounds for known multi-viewport bugs (macOS focus/resize) are listed in
inventory 07 §1.1. Upgrade to 0.37 once released (it needs `default_fonts`
explicitly).

## ADR-009 — Prefer existing crates when they meet requirements
(Revised 2026-10-14 per user direction: "use existing crates if they meet
your needs; if not, write it yourself.")

For every component, first evaluate existing crates against:
1. **Forensic completeness**: deleted/unallocated data, corrupt-input
   tolerance (recover what is readable instead of failing).
2. **Robustness**: no panics on hostile input (check with our fuzz/proptest
   harness before adoption).
3. **API fit**: works over shared positional readers (`ReadAt`) for
   multi-threaded ingest, or can be wrapped cheaply.
4. **Licence** (ADR-010), **maintenance** and **maturity**.

Adopt when the crate passes. Otherwise write it ourselves and keep the crate
as a dev-only differential oracle. Record each evaluation here.

Evaluations:
- **NTFS: `ntfs` 0.4.0 (Colin Finck, MIT/Apache) → oracle only.** It cannot
  read an `$MFT` that has an attribute list (fragmented MFTs on real disks),
  treats fixup mismatches (torn records) as hard errors, has no LZNT1 /
  compressed-attribute support and no `$I30` slack recovery, and re-reads
  MFT record 0 on every `file()` call. Our `pm-fs-ntfs` is in-house; `ntfs`
  is a dev-dependency for differential tests.
- **FAT: `fatfs` 0.3.6 → test-fixture writer only.** It has no
  deleted-entry or orphan recovery.
- **EWF: in-house.** The candidates were young (2026) single-author crates.
  Verified against libewf's `ewfacquire`.
- **EXIF: in-house (`pm-modules::exif`).** `kamadak-exif` 0.6 needs the
  whole TIFF in memory (fails criterion 3: a TIFF's IFDs may sit at the end
  of a multi-GB file); the Picture Analyzer only needs six tags, read with
  random access over `ReadAt`.
- **To evaluate when reached**: `notatin` (registry), `evtx`, `lnk`, `cfb`,
  `plist`, `mail-parser`, `outlook-pst`, `zip`, `sevenz-rust2`, `rars`,
  `tantivy`, `yara-x`, `nom-exif`/`kamadak-exif`, `file-format`/`infer`,
  `hayro`, `calamine`. These are expected to be adopted (inventory 07 §20).

## ADR-010 — Licence policy
The product is AGPL-3.0-or-later (changed from Apache-2.0 on 2026-10-18,
to keep hosted and multi-user forks open). Autopsy-derived code stays
attributed in `NOTICE`; Apache-2.0 code may be included in (A)GPLv3 work.
`pm-plugin-api` and `pm-plugin-sdk` stay Apache-2.0 and must depend only
on permissively licensed crates, so plugins can use any licence.
Dependencies must be AGPLv3-compatible: no GPL-2.0-only, no proprietary;
MPL-2.0 is allowed unmodified. Release archives ship third-party licence
texts.
Enforce with `cargo-deny` (`deny.toml`) when it lands in the gate.

## ADR-011 — wasmtime pinned to yara-x's major
`yara-x` 1.21 requires wasmtime ^45. The WASM plugin host uses the same major
to avoid shipping two wasmtime copies. Revisit when yara-x moves.

## ADR-012 — Autopsy parity through differential testing
Where Autopsy's behaviour comes from Autopsy (volume layout, FS trees,
names, orphans), we match Autopsy 4.15 output (`mmls`/`fls`/`istat`) on
generated images via `pm_testkit::reftools`. Where we recover *more* (full deleted
long names, orphan content), the differential test accepts the Autopsy form as
an alternative and the improvement is documented in the test.

## ADR-013 — Analyzers: core parsers in Rust, plugins in WebAssembly
(Decided with the user 2026-10-14.)

* **Built-in analyzers are Rust.** This covers everything Autopsy ships,
  including the Jython Android/GPX modules and ALEAPP/iLEAPP-style app
  parsers. Simple "find DB → query → map columns" parsers may be
  table-driven inside Rust (a shared query/mapping engine), but that is an
  implementation detail, not a user-facing format.
* **Third-party extensibility is WebAssembly** (wasmtime component model,
  versioned WIT world, pinned to yara-x's wasmtime major per ADR-011).
  Plugins get read-only file content and metadata through host calls, emit
  artifacts/analysis results through the host (which batches DB writes and
  schedules work in parallel), are sandboxed (no ambient FS/network), and
  have fuel/memory limits. Ingest modules, report modules and content
  viewers are plugin kinds. Ship templates (Rust first) and a scaffold.
* **No embedded Python.** Autopsy's Jython plugin API is not reproduced.
  External tools (aLEAPP/iLEAPP, Plaso, Volatility) may still run as
  subprocesses with their output imported, as Autopsy does.

## ADR-014 — Scale target: tens of terabytes, hundreds of millions of files
(User requirement, 2026-10-14: "should work for filesystems up to multiple
terabytes, even tens of terabytes … search and everything stays performant".)

Design budgets (to be enforced by benchmarks in `benches/` and a
`scale-tests` feature; see PLAN "Scale" track):
* **Evidence size**: 50 TB images, 500 M files across a case; single
  directories with ≥ 1 M entries; MFTs of tens of GB.
* **Memory**: bounded by working set, not by evidence size. No structure
  may be O(number of files) in RAM unless it is ≤ ~16 bytes per file and
  optional. The in-memory `pm-index` catalogue is a stop-gap and is replaced
  by DB-backed queries (M2) before ingest lands.
* **Enumeration**: NTFS uses a sequential bulk MFT scan (large sequential
  reads, parsed in parallel chunks) to build the file table, not a recursive
  directory walk. Other filesystems walk directories with read-ahead.
  Target ≥ 200 k files/s loaded on SSD.
* **Database**: bulk loads in large transactions with prepared statements;
  secondary indexes are built after the bulk load; WAL; queries always go
  through an index; UI listings use keyset pagination (never OFFSET, never
  "load all"); counts for tree badges are precomputed or cached.
* **Search uses Tantivy** (user direction): content/keyword search
  (replacing Autopsy's Solr) *and* file-name/path search, using an n-gram
  tokenised Tantivy index for substring queries over 100s of millions of
  names (smaller and faster than SQLite FTS5 trigram at this scale). No LIKE
  scans on large tables. Analysis views
  (Timeline counts, Discovery groups, Data Source Summary) read
  pre-aggregated tables updated at load/ingest time, never full scans on
  interaction.
* **UI**: every operation that can touch more than ~10 k rows runs off the
  UI thread with progress and cancellation; tables are virtualised and
  fetch pages on demand.
* **IO**: positional reads with large, aligned requests; caches bounded
  (sharded LRU); hashing and ingest parallel per file with back-pressure.

## ADR-015 — Ingest framework design

**Status:** accepted (2026-10-14)

**Context.** Autopsy's ingest framework (inventory 03 §1) defines the module
contract that every analyser is written against: four module kinds, one
module instance per worker thread, pipeline order rules, tiers, file
filters, re-injection of derived files, batched persistence of
`known`/MIME/hashes, inbox messages and job records. Its scheduler expands
directories lazily under one global lock and saves each file through the
`AbstractFile` object.

**Decision.** `pm-ingest` keeps the externally visible semantics and changes
the machinery:
* **Contract.** `ModuleFactory::start(job, settings) -> Arc<dyn ModuleJob>`
  creates explicit per-job shared state (replacing Autopsy's static
  reference-counter maps). `ModuleJob` hands out one `FileModule` per worker,
  optional data-source modules for a numbered stage, and gets `finish()`
  once after every instance has shut down (summary messages). Settings are
  `serde_json::Value` per module id, so a module can version them.
* **Scheduling.** Files are streamed from the case DB with keyset paging, one
  pass per Autopsy root priority class (`Users`… first, `pagefile`/
  non-file-system content last). Memory is bounded by the page and queue
  sizes, never by the file count. Derived files go into a priority queue
  that workers drain first.
* **Execution.** N workers, each with its own pipeline instances in pipeline
  order. Content is opened once per file and shared by the modules; the
  first bytes are cached for signature checks.
* **Persistence.** Modules record MIME types, hashes, `known` and artifacts
  through the job context. One writer thread applies them in short
  transactions (≤ 5 000 operations), so other writers such as tagging are
  never locked out for long. Artifact posting is idempotent (same file, type
  and attributes ⇒ no duplicate), which gives re-ingest Autopsy's
  `artifactExists` behaviour.
* **Stages.** Data-source modules declare a stage; stage *n + 1* starts when
  every task of stage *n* has finished (Autopsy's tiers, generalised).
* **Errors.** A module error is recorded and the pipeline continues. A
  `start` failure aborts the job before any data is touched.

**Consequences.** Modules are plain Rust structs that can be unit-tested
against an in-memory case. The same contract is exposed to WebAssembly
plugins (ADR-013) through a thin adapter.

## ADR-016 — Derived files: storage, encoding, ids and re-injection

**Status:** accepted (2026-10-15, derived-and-carving track)

**Context.** Archive members, embedded images and carved files become case
files that are analysed like any other (inventory 03 §1.9, §2.4, PhotoRec
§3). Autopsy stores extracted bytes XOR1-encoded under
`<case>/ModuleOutput/<module>/<parent id>/…`, records the encoding in
and re-queues new files as high-priority tasks
(`addFilesToJob`).

**Decision.**
* **No schema change.** Derived files are `files` rows with `origin`
  derived/carved/local, `derived_from` = source file, `local_path`
  (relative to the case folder, `/`-separated, resolved without `..` or
  absolute components) or `layout` (image byte ranges for carved files).
* **XOR1 on disk, like Autopsy.** Written files carry Autopsy's 32-byte header
  `KAAMOS_CONTAINER_XOR1_xxxxxxxxxxxxx` followed by every byte XOR 0xCA, so
  antivirus software does not quarantine malware extracted into case
  folders, and Autopsy-written module output reads the same way. The
  encoding is recognised from the header when content is opened (Autopsy
  `EncodedFileUtil.getEncoding`), so no column is needed; unencoded files
  read as they are. Local files with absolute paths are never decoded.
* **One file-id allocator per open database** (`CaseDb::file_ids`, atomic,
  initialised from `MAX(id) + 1`). The loader, the ingest writer and
  modules reserving ids for derived trees all use it; `Bulk::add_file`
  never lets SQLite pick an id, so reserved ids cannot be taken.
* **Insertion through the job's single writer, synchronously.**
  `JobContext::add_derived_files` sends the rows to the writer, which
  commits immediately and replies with the records; the files are then
  pushed to the front of the job's high-priority queue. A worker only
  stops when both queues are empty, no task is running and no producer or
  stage-1 data-source module holds the job open, so the job ends only
  after all derived work is done. The producer streams only ids below the
  allocator value at job start, so new files are never processed twice.
  Files added by a later data-source stage get an extra file phase
  (Autopsy drops them with an error).

**Consequences.** Re-ingest sees derived files as ordinary (`Last` class)
files. Modules that re-extract (Embedded File Extractor) must not re-add
files that already exist, or they would be analysed twice.

## ADR-017 — Schema v2: Autopsy's data model on separate id spaces

**Status:** accepted (2026-10-17, WP-0)

**Context.** Hosts/persons, OS accounts, communication accounts, timeline
events and aggregate scores were missing (PARITY §0.4), which blocked
OS Accounts, Communications, the Score tree, tag scoring and artifact
timeline events. Autopsy keys all of these on one  id space;
postmortem has separate `files`, `artifacts` and `os_accounts` tables
(ADR-002).

**Decision.** Migration `0002_schema_v2.sql` ports Autopsy 4.12's tables and
semantics (`CaseDatabaseFactory`, `OsAccountManager`,
`OsAccountRealmManager`, `HostManager`, `CommunicationsManager`,
`ScoringManager`, `TaggingManager`, `TimelineManager`):
* Polymorphic references name the table instead of an object id:
  `scores(obj_kind, obj_id)` with kind file/artifact/OS account; an
  analysis result scores its `source_artifact_id` if set, else its file;
  `event_descriptions(file_id, artifact_id)`; account relationships point
  at the source artifact.
* Significance codes now sort like Autopsy's ordinals
  (`Notable > None > LikelyNotable > LikelyNone > Unknown`); v1 rows are
  remapped and scores backfilled by the migration.
* Aggregate scores are maintained in the same transaction as the result
  or tag that changes them (raise on add, recompute on remove), so the
  Score tree, S column and Discovery never aggregate results on the fly.
* Notable tags change the score, never `files.known` (Autopsy behaviour; v1
  set `known = 2`).
* OS account instances keep only the most significant kind per data
  source (Autopsy's read semantics, stored once).
* Ported from Autopsy's own `OsAccountTest` (13 cases) plus scoring, tag-set,
  event, communications and v1 → v2 migration tests.
* Fixed Autopsy bug: after a realm update Autopsy looks up "another realm with
  the same address" by the realm *name*, so that merge never happens; we
  look up the address.

**Not yet ported:** host addresses API (tables exist), data-artifact and
analysis-result ingest pipelines (WP-0 follow-up in `pm-ingest`).

## ADR-018 — File-system timeline events are virtual

**Status:** accepted (2026-10-17)

**Context.** Autopsy writes four  rows per file (MACB), each with
an `event_descriptions` row repeating the path. With hundreds of millions
of files (ADR-014) that is billions of rows and several copies of every
path.

**Decision.** Only artifact events are stored (`events` +
`event_descriptions`). File-system events are read from the time columns
of `files` through indexes (the timeline WP adds the missing
`atime`/`ctime`/`crtime` indexes and pre-aggregated per-unit counts).
The event type ids are Autopsy's, so both sources merge into one timeline.

**Consequences.** No load-time cost for FS events; the timeline counts
query unions two sources; "tagged"/"hash hit" filters for FS events join
`file_tags`/hash-hit artifacts instead of reading description flags.

## ADR-019 — Directories get no MIME type

**Status:** accepted (2026-10-17, core-file-modules track)

**Context.** Autopsy's `FileTypeDetector.getMIMEType` gives every object
that is not a regular file (`!isFile()`) `application/octet-stream`, and
the ingest scheduler always processes directories, so Autopsy stores
octet-stream on every directory. Its own `IngestFileFiltersTest` asserts
the opposite (directories outside the filter have no MIME type), see
inventory 08 §3.

**Decision.** File Type Identification leaves directories and virtual
directories (`$OrphanFiles`, `$Unalloc`, derived folders) without a MIME
type. Every other object is typed as in Autopsy: regular files by
detection; empty files, unallocated blocks, slack under 4 096 bytes and
non-regular objects (FAT/exFAT virtual files, symlinks, devices) as
`application/octet-stream`. Directory traversal is never filtered.

**Consequences.** No useless writes for millions of directories; MIME
views, filters and rules (which only match files) are unaffected.

## ADR-020 — Keyword search on Tantivy: index layout and parity decisions

**Status:** accepted (2026-10-17, search track)

**Context.** Autopsy's Keyword Search stores every 32 KB chunk three times
in an out-of-process Solr core (`text` with term vectors, a lower-cased
`content_str` single term for regex/substring, `content_ja`), runs
substring and regex queries as `/.*x.*/` scans over those terms, and
keeps inline-search hits in memory until the ingest job ends (inventory
05 §1.6–1.8, §4.1). Some of its behaviour is buggy.

**Decision.**
* **Two Tantivy indexes per case** in `<case>/Index/` (pure Rust; the
  `columnar-zstd-compression` default feature is off to avoid a C
  dependency): `content` (chunk documents with word tokens + positions,
  distinct lower-cased trigrams and the stored text; one summary
  document per object with its name/label and chunk count) and `names`
  (positional trigrams of each file's name and path). An `Index/VERSION`
  file wipes indexes of another layout.
* **Candidates, then exact verification.** Exact queries are phrase
  queries on words; substring and regex queries use trigram formulas
  derived from the regex HIR (Cox's trigram index), or a scan when no
  trigram is required. Every candidate is verified with the same
  `Matcher` the ingest module uses, so ad-hoc and ingest hits agree. The
  name index answers substrings exactly with positional phrase queries.
* **One regex engine** (Rust `regex`, Unicode classes, case-insensitive)
  for ingest and ad hoc; Java syntax differences used by keyword lists
  (`\<`, `\>`, `\Q…\E`) are translated; look-around and back-references
  are reported as invalid instead of being silently skipped.
* **Results are posted per file**, not at job end (bounded memory;
  cancelled jobs keep what was found). Re-runs are idempotent (keyword
  hit identities read once per file; accounts de-duplicated by the
  communications manager; objects re-indexed in place).
* **Kept for parity:** the IIN length set `{16, 17, 18, 29}` (Autopsy's
  typo; its own vectors pass either way because of the range override
  order), the credit-card regex's lazy separator (a spaced number only
  matches its first twelve digits), the built-in regexes verbatim
  (e-mail TLDs 2–4 letters, URL quirks), metadata-only entries for known
  files without a name search, `.txt` and carved-`.txt` special cases,
  Autopsy's metadata block and key priorities.
* **Fixed:** exact matches re-examine tokens after a partial multi-word
  match; snippets may show a chunk's last character; BIN ranges pad a
  blank end from the raw start with 9s; metadata dates are UTC (Autopsy
  uses the JVM time zone); lower-casing is locale-independent; no
  English stop words (Autopsy's index cannot find "the"); literal
  keywords such as `C++` are never rejected as invalid regexes; user
  keywords may be any length; per-hit inbox messages honour the list's
  "send messages" flag again (first 20 per term, as Autopsy's
  `QueryResults` did).
* **Page numbering:** the name is page 0 and chunks are 1…N in both hit
  bookkeeping and the index (Autopsy mixes 0-based inline chunk ids with
  1-based Solr ids). `KAAMOS_KEYWORD_SEARCH_DOCUMENT_ID` uses Autopsy's
  `<objId>_<chunk>` form; artifacts are `ObjRef::Artifact` with key
  −id, as Autopsy numbers them below zero in Solr.

**Consequences.** No JVM or Solr process, no network, smaller indexes
(text stored once, no term vectors), substring/regex queries that touch
only candidate chunks. Japanese morphological analysis (`content_ja`),
language detection, OCR and the "Extract Unique Words" report are not
ported yet. Multi-user cases need an index service (inventory 05 §1.14);
a second process opens the indexes read-only.

## ADR-021 — MCP server: rmcp transports over a synchronous tool engine

**Status:** accepted (2026-10-17, mcp track)

**Context.** BEYOND-AUTOPSY §8 asks for Autopsy's 17 MCP tools plus
search, pivot and timeline tools, served over stdio and streamable HTTP,
safe against prompt injection planted in evidence, and cheap on cases with
hundreds of millions of files (ADR-014). The case crates are synchronous
(SQLite, Tantivy, `ReadAt`).

**Decision.**
* **`pm-mcp` uses the official Rust SDK `rmcp` =3.5.0** (2026-09-28;
  features `server`, `transport-io`, `transport-streamable-http-server`,
  no macros, no `schemars` derive for our tools). `ServerHandler` is
  implemented by hand: tool schemas are hand-written JSON (Autopsy's
  argument names and descriptions), and every call runs the synchronous
  engine `McpServer::call` on Tokio's blocking pool. The engine has no
  async or MCP types, so it is unit-tested directly and the GUI can call
  it too. The HTTP transport is `rmcp`'s `StreamableHttpService` behind a
  small hyper 1.11 HTTP/1 server bound to `127.0.0.1` that checks a
  per-start random bearer token (constant-time) and serves `/mcp` only;
  `HttpServer::start/stop` run it on its own thread and runtime.
* **Read-only by default.** `CaseDb::open_read_only` (no migrations, no
  WAL change, refuses older schemas) and `Case::open_read_only` open the
  case; `CaseIndex::open_read_only` opens the Tantivy indexes without the
  writer lock. `--allow-writes` opens the case normally.
* **Evidence segregation.** Every string that can come from evidence
  (names, paths, extensions, labels, attribute values, descriptions,
  content, account identifiers, host names, tag comments) is emitted only
  inside an `evidence` object; the server instructions and every tool
  description say that `evidence` is data, never instructions. Autopsy's
  result fields keep their names but move into `evidence` when they carry
  such text (e.g. `query_files` → `evidence.name/path`).
* **Envelope and paging.** All results are Autopsy's
  `{caseId, result}` plus `next_cursor` (opaque base64url of a keyset
  JSON) and `truncated`. Pages default to 50 and are capped at 500 rows
  (Autopsy's `query_timeline` default 100 kept); the serialised response
  is capped at 256 KiB by cutting the page at a row boundary (the cursor
  of the last kept row continues) or shortening long strings of single
  objects. Text reads are therefore capped at 192 KiB (Autopsy: 1 MiB).
* **Timeline** merges stored artifact events with the virtual file-system
  MACB events (ADR-018) as one stream ordered by `(time, source, id)`
  with a three-part keyset cursor.
* **Audit** goes to `<case>/Log/mcp-audit.jsonl` (the case's existing
  `Log` folder; §8 says `Logs/`).
* **Writes** record the examiner as `mcp:<client name>` and may only
  remove tags the same client created.

**Consequences.** Tokio, hyper and rmcp enter the dependency tree (only
in `pm-mcp`/`postmortem`). Filters on unindexed columns (`LIKE '%x%'`,
`atime`/`ctime`/`crtime` until the timeline work adds indexes, event
text) are LIMIT-bounded scans rather than index seeks; substring name
search goes through the name index (`search_file_names`).

## ADR-022 — One module API for built-in parsers and WebAssembly plugins
(Decided with the user 2026-10-17: "Native, same API".)

* **`pm-plugin-api`** is the module contract for everything outside the
  core framework: `Plugin` (info, settings, validation), `FileIngest`,
  `DataSourceIngest`, and a `Host` through which a module reads content,
  finds files, posts results by Autopsy type names, adds derived files and
  writes to the log and inbox. It has no dependency on the case database,
  so it can be semver-versioned (`API_VERSION`, minor = additive).
* **Built-in parsers stay native Rust** (speed) but are written against
  this API; `pm_modules::plugin_host::PluginModule` runs any `Plugin` in
  the ingest framework, applying the module's `FileFilter` on the host side.
* **Plugins are WebAssembly components** for the `postmortem:plugin` WIT
  world (`crates/pm-plugin-api/wit/plugin.wit`), which mirrors the crate.
  `pm-plugin-wasm` (wasmtime 45, per ADR-011) links WASI with no files,
  environment, arguments or network, charges fuel per call, caps memory
  per instance, runs one instance per worker and recreates it after a
  trap. Guests use `pm-plugin-sdk` (wit-bindgen) and implement the same
  Rust traits, exported with `export_plugin!`.
* **Distribution**: a plugin is a folder with `plugin.toml` (API version,
  component path, limits) and the `.wasm`, under `~/postmortem/Plugins`
  (or `$POSTMORTEM_PLUGINS`). Identity and capabilities come from the
  component's own `info()`; ids may not collide with built-ins.

**Consequences.** Any parser can later ship as a plugin with little
change; results from plugins are attributed to the plugin's display name;
API 0.2 added `post_communication` (accounts and relationships written
with the result) and `define_attribute_type` / `define_artifact_type`;
plugins built against 0.1 load unchanged (the host only gained
functions). The Email Parser is the first built-in module on this API.

## ADR-023 — Everything reads the case database; architecture review 2026-10-18
(Owner directive 2026-10-18: "The timeline should use database and not
memory… basically everything should use database.")

**Decision.** Every view and tool (Timeline, Image Gallery, Discovery,
Data Source Summary, searches, MCP, reports) reads the case database. No
feature walks the evidence into memory. Evidence is opened only through
`pm_case::Case` (`filesystem()`, `image_source()`, cached per data
source) for content reads.

Done so far: artifact timeline events are written with each artifact
(`pm-casedb/src/artifact_events.rs`, Autopsy `TimelineEventTypes`); the
Timeline window runs on `pm_timeline::db::DbTimeline` (indexed queries
per range and filter, schema v3 adds the time-column indexes); the Image
Gallery lists pictures from the DB.

**Review findings and plan (in order).**
1. **[done 2026-10-18, de1801a]** The GUI opens each image a second time (`PostmortemApp::sources`,
   `pm_evidence::open`) beside `Case`'s cached evidence, and builds a
   full in-memory file index (`SharedIndex`/`pm-index`) eagerly. Move the
   Data Source Summary and Discovery to DB queries, then delete
   `sources`, `SharedIndex` and the `pm-index` file walk (keep
   `FileCategory` in a shared crate).
2. `pm-ui/src/app.rs` (~3,400 lines) holds every tool's state and ~60
   manual `invalidate()`/`refresh()` calls. Add a **case change bus**
   (Autopsy `Case.Events`): `CaseDb` bumps per-domain generation counters
   (files, artifacts, tags, events, data sources, reports, accounts) on
   commit; views refresh when their counter moves. Then split `app.rs`
   into feature modules that own their state.
3. UI `Task`s share rayon's global pool with heavy work (a WASM compile
   once starved the UI). Give UI queries their own small pool.
4. `pm-casedb/src/mcp.rs` has become the general read/query layer
   (timeline, file filters, keyset paging) used beyond MCP. Rename it
   (`query`), drop the `mcp_` prefixes; MCP-specific shaping stays in
   `pm-mcp`.
5. Two module APIs coexist (`ModuleFactory` and `pm-plugin-api`);
   continue moving built-in modules onto `pm-plugin-api` (ADR-022).
6. Explain the ~3 GB peak resident during ingest of a 15 GB image
   (`postmortem ingest` now reports heap vs mapped pages).

**Consequences.** Memory no longer grows with the number of files for
browsing features; MCP, reports and the GUI see the same data; old
in-memory code paths and their flaky test waits disappear.

## ADR-024 — Multi-user on PostgreSQL through a dialect layer
(Owner decision 2026-10-18.)

**Decision.** Single-user cases stay on SQLite. Multi-user cases use
PostgreSQL, as Autopsy does. `pm-casedb` keeps one public API (`CaseDb`
and its query types); SQL is produced through a thin dialect layer
(placeholders, upserts, `RETURNING`, type names, `LIKE`/collation and
index DDL differences) over two backends. Refactor `pm-casedb` toward
this before more SQL is added; new queries go through the layer.
Postgres tests run against the official image via Docker/Podman.

**Alternatives rejected.** A postmortem case server in front of SQLite
(new server component, no shared-database deployment like Autopsy's);
postponing multi-user (the owner keeps it in scope).

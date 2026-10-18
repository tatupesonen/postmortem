# postmortem — plan

postmortem is a pure-Rust port of Autopsy, aiming for full feature parity
plus better performance and usability. The parity reference is
`docs/inventory/*.md`; how work proceeds is in `docs/LOOP.md`; decisions are
in `docs/DECISIONS.md`.

## Goals

1. **Parity.** Everything Autopsy does, including multi-user cases,
   auto-ingest, command-line ingest, the MCP server and plugins. Exceptions
   (removed Autopsy features such as STIX) are noted in the inventories.
2. **Speed.** Ingest is bounded by disk throughput, not by locks or the
   database. The UI never blocks and scrolls millions of rows smoothly.
3. **Usability.** egui UI with dockable panes and real OS pop-out windows
   (multi-viewport); keyboard-first navigation; everything searchable and
   sortable; nothing modal unless it must be.
4. **Trustworthy.** Panic-free parsing of hostile evidence, reproducible
   results, golden regression tests, read-only evidence access.

## Architecture

Current crates (planned ones are in *italics* below the diagram):

```
 postmortem (bin: GUI, `ingest`, `mcp`)
   ├── pm-ui (egui)            pm-mcp (MCP server)          pm-report (HTML, XLSX, KML, …)
   │      │                        │                            │
   │   analysis read-side: pm-timeline (DB timeline)  pm-geo  pm-discovery  pm-search (Tantivy)
   │      │
   ├── pm-modules (built-in ingest modules) ── pm-plugin-api / pm-plugin-wasm / pm-plugin-sdk
   │      │      parsers: pm-browser pm-registry pm-evtlog pm-lnk pm-prefetch pm-winartifacts
   │      │               pm-email pm-archive pm-carve pm-filetype pm-encryption pm-hashdb pm-rules pm-text
   ├── pm-ingest (jobs, pipelines, writer thread, inbox)
   ├── pm-case (case folder, evidence re-open, loader) ── pm-casedb (SQLite; PostgreSQL planned, ADR-024)
   ├── pm-evidence ── pm-image  pm-volume  pm-fs (+ pm-fs-ntfs, -fat, -exfat, -ext)
   └── pm-core (ReadAt, decoding, time, units)          pm-testkit (fixture builders)
```

*Planned:* `pm-server` / `pm-worker` (multi-user, auto-ingest),
`pm-centralrepo`, `pm-fs-hfs`, `pm-fs-apfs`, `pm-fs-iso`, lower-priority
file systems, an ESE reader for Edge legacy/WebCache and SRUM. Not
planned any more: `pm-app` (application services live in `pm-case` and
`pm-ui`), `pm-cli` (the `postmortem` binary has `ingest` and `mcp`
subcommands), `pm-comms`/`pm-summary` (in `pm-casedb` queries and
`pm-ui`), `pm-artifacts` (split into `pm-registry`, `pm-evtlog`,
`pm-lnk`, `pm-prefetch`, `pm-winartifacts`, `pm-browser`), `pm-mod-*`
(one `pm-modules` crate), `pm-index` (in-memory file walk, removed
2026-10-18 by ADR-023).

Dependency rules: arrows point down only. Parsers know nothing about
cases or the database. Ingest modules depend on `pm-ingest` (or
`pm-plugin-api`) + parsers + `pm-casedb`. Every view reads the case
database (ADR-023); the UI never runs blocking IO on the UI thread.

### Data flow

1. **Add data source** (`pm-case` loader over `pm-evidence`): open the
   image → detect the volume system → detect the file system per
   partition (or at offset 0) → walk the tree in parallel → bulk-insert
   `files` rows in large transactions with deferred secondary indexes.
   Unallocated space and orphan files become virtual files.
2. **Ingest** (`pm-ingest`): a data-source stage and a multi-threaded
   file pipeline; modules are factories creating per-worker instances
   (natively or through `pm-plugin-api`). Results go through one writer
   thread that batches them into transactions; each data artifact posts
   its timeline events in the same transaction.
3. **Browse** (`pm-ui`, `pm-mcp`, `pm-report`): indexed, keyset-paged
   queries on the case database, run off the UI thread (ADR-023).
   Content is read through `Case::filesystem()` / `image_source()`.

### Case storage (ADR-002, ADR-017)

- Case folder: `case.pmcase` (TOML metadata), `case.db` (SQLite WAL),
  `Index/` (Tantivy content and file-name indexes), `Export/`, `Reports/`,
  `ModuleOutput/`, `Temp/` (external-viewer copies, map tiles), `Log/`.
- Tables (schema v3): `files` (one row per file, directory, derived,
  carved or virtual file: name, parent path, metadata, flags, sizes, the
  four times, hashes, MIME, extension, known status, location),
  `data_sources`, `volume_systems`, `volumes`, `file_systems`,
  `artifact_types`, `attribute_types`, `artifacts` (data artifacts and
  analysis results with score, conclusion, justification,
  configuration), `attributes` (typed value columns), `file_attributes`,
  `tag_names`/`tag_sets`/`file_tags`/`artifact_tags`, `scores`
  (aggregate per object), `hosts`, `persons`, host addresses,
  `os_accounts` (+ realms, instances, attributes), `accounts`,
  `account_types` and `account_relationships` (communications),
  `event_types`/`event_descriptions`/`events` (timeline; file-system
  events are the `files` time columns, ADR-018), `ingest_jobs` /
  `ingest_job_modules`, `reports`, `examiners`, `meta`.
- Ids live in separate spaces per table (ADR-017). Indexes follow the UI
  query patterns; enums are integers; text search is in Tantivy.
- SQLite now; PostgreSQL for multi-user cases through a dialect layer
  (ADR-024).

### UI

- eframe/egui (0.36): menu bar, toolbar, tree | result tabs | content
  viewer panels. The content viewer, timeline, gallery, discovery and
  other tools open in their own OS windows (egui deferred viewports).
- Virtualised tables (`egui_extras`) over keyset-paged DB queries; all
  database and evidence work runs as background tasks (`pm-ui/src/task.rs`).
- `egui_kittest` drives UI tests (`crates/pm-ui/tests/ui_flows.rs`);
  screenshots with `PM_UI_SCREENSHOT_DIR`.
- Planned (ADR-023): a case change bus instead of manual refreshes, and
  splitting `app.rs` into feature modules.

### Multi-user (from inventory 06 §16)

PostgreSQL is the only required server. A single `pm-server` provides
coordination (advisory locks plus lease tables in place of ZooKeeper), events
(LISTEN/NOTIFY plus a persisted event log in place of ActiveMQ), a shared
Tantivy index service (in place of Solr) and the auto-ingest job queue
(`SKIP LOCKED` leases). `pm-worker` provides auto-ingest nodes. Shared case
folders sit on a network filesystem as in Autopsy.

### Plugins

Per ADR-013, all built-in analyzers (including Autopsy's Jython Android/GPX
modules) are Rust. Third-party extensions are WebAssembly components
(wasmtime, versioned WIT world) that are sandboxed, fuel- and memory-limited,
and access evidence and emit results only through host calls. There is no
embedded Python.

## Crate map

| Crate | Purpose | Status |
|-------|---------|--------|
| `pm-core` | ReadAt sources, bounded decoding, time, units | done |
| `pm-testkit` | fixture builders (demo disk, NTFS/FAT/ext images, artefacts) | done, grows with features |
| `pm-image` | raw/split, EWF v1, VMDK, VHD, VHDX, QCOW2 (EWF2, DMG, AFF4 not yet) | done except EWF2/DMG/AFF4 |
| `pm-volume` | MBR/EBR, GPT (APM, BSD, LVM not yet) | MBR + GPT done |
| `pm-fs` | FileSystem trait, ExtentSource, walker | done |
| `pm-fs-ntfs`, `pm-fs-fat`, `pm-fs-exfat`, `pm-fs-ext` | file systems | done (NTFS extras pending) |
| `pm-evidence` | image → volume system → file systems | done (other data sources live in `pm-case`) |
| `pm-casedb` | schema v3, migrations, typed API, query layer, artifact timeline events | done (SQLite); PostgreSQL planned |
| `pm-case` | case folder, metadata, loader, evidence re-open, delete | done (event bus pending, ADR-023) |
| `pm-ingest` | jobs, pipelines, writer thread, inbox, progress | done (profiles pending) |
| `pm-modules` | built-in ingest modules (Autopsy's core set, Recent Activity, email, GPX, carving, …) | most done, see PARITY |
| `pm-filetype` | MIME detection (Tika signatures + custom types) | done |
| `pm-hashdb` | hash set formats + sorted-index lookup, `.kdb` read/write | done |
| `pm-text`, `pm-search` | text extraction; Tantivy content and file-name indexes | done (OCR, language detection pending) |
| `pm-registry`, `pm-evtlog`, `pm-lnk`, `pm-prefetch`, `pm-winartifacts`, `pm-browser` | artefact parsers | done for the formats listed in PARITY |
| `pm-email` | MBOX, EML, vCard, threading (PST/OST pending) | partial |
| `pm-archive`, `pm-carve`, `pm-encryption`, `pm-rules` | archives, carving, encryption detection, interesting-file rules | done |
| `pm-geo` | waypoints/tracks/routes/areas, GPX, KML | done |
| `pm-timeline` | event model, calendar bins, DB timeline (`db.rs`) | done |
| `pm-discovery` | file discovery filters/grouping over files of a type from the case DB | first cut |
| `pm-report` | HTML, XLSX, files text, body file, KML, tagged hashes | done (portable case, CASE/UCO pending) |
| `pm-plugin-api`, `pm-plugin-wasm`, `pm-plugin-sdk` | plugin API, wasmtime host, guest SDK (ADR-022) | done (ingest kind) |
| `pm-mcp` | MCP server (ADR-021) | done |
| `pm-ui` | egui application | most of the UI; see PARITY |
| `postmortem` | binary: GUI, `ingest`, `mcp` | done |
| `fuzz/` | cargo-fuzz targets (outside the workspace) | started |

## Milestones

The order favours an early end-to-end vertical slice (open image → browse →
view) and then widens. Each milestone ends with: gate green, fuzz smoke run
for new parsers, benchmarks recorded, PROGRESS log entry.

### M0 — Foundations ✅
- [x] Workspace, toolchain pin, lints, loop protocol
- [x] `pm-core`: ReadAt sources, Bytes decoding, timestamps, units
- [x] `pm-volume`: MBR/EBR + GPT with Autopsy-style layout
- [x] `pm-image`: raw + split raw, format sniffing
- [x] `pm-fs`: trait, ExtentSource, walker
- [x] `pm-fs-fat`: FAT12/16/32 with deleted-file recovery

### M1 — Evidence layer (vertical-slice subset first)
- [x] `fuzz/` crate with targets for volume, FAT; smoke script `scripts/fuzz-smoke.sh` (seeds in `fuzz/seeds/`)
- [x] `pm-image` EWF v1 (E01/s01): segments (E01…ZZZ), zlib/stored chunks, >2 GiB offset overflow, stored MD5/SHA-1, header2/header metadata, error2; verified against ewfacquire (9 format variants, multi-segment) + fuzz target
- [ ] `pm-image` EWF2 (Ex01/Lx01); L01 logical evidence is done (`pm-case/src/l01.rs`)
- [x] `pm-fs-ntfs`: boot, MFT (incl. fragmented $MFT via attribute list), fixups (torn-tolerant), runlists, resident/non-resident, LZNT1, sparse, init size, ADS, $SI+$FN times, deleted via parent map, $I30 slack, orphans, unallocated clusters. In-house `NtfsBuilder` (pm-testkit) validated by Autopsy; differential vs `fls` + `ntfs` crate oracle; fuzz target
- [ ] NTFS extras: $Secure/owner SIDs, reparse points/junctions, $UsnJrnl/$LogFile parsing, EFS flagging, WOF/compact compression, hard-link enumeration, bulk MFT enumeration fast path
- [ ] `pm-evidence`: probe chain (image → VS → FS registry), unallocated-space virtual files, partitions with unknown FS, "no VS" fallback, logical files & folders DSP, local disk DSP (read-only)
- [x] `pm-fs-exfat`: entry sets with checksum validation, deleted sets, NoFatChain/valid-length, UTC offsets (Autopsy gets these wrong), alloc bitmap, orphan scan; macOS-generated fixture + fls differential + fuzz
- [x] `pm-fs-ext` (ext2/3/4): extents (any depth, uninit), indirect blocks, inline data, fast symlinks, dirent-slack deleted names, bitmaps incl. uninit groups, nanosecond/crtime times; fixtures via `fstool` (pinned dev-dep) + fls differential
- [ ] `pm-fs-hfs` (HFS+), `pm-fs-apfs`
- [ ] `pm-fs-iso` (ISO9660/Joliet/RR, UDF)
- [x] `pm-image` VMDK (sparse + flat + descriptor), VHD (fixed/dynamic/diff), VHDX, QCOW2 (done, image-formats track), DMG (UDIF) — except DMG (UDIF), still open
- [ ] `pm-volume` APM, BSD disklabel; detect-only for LVM, BitLocker, LUKS, Core Storage
- [ ] slack-space content for FAT/NTFS (Autopsy "-slack" files)
- [ ] XFS, Btrfs, YAFFS2 (lower priority; may move to M13)

### Cross-cutting — test parity (inventory 08)
- [ ] Work through the 126-item parity checklist in `docs/inventory/08-autopsy-tests.md`, ticking items there as each area lands (credit-card vectors → M5, correlation normalisers → M8, ingest filters → M4, …)
- [ ] `cargo-deny` licence/ban checks in the gate (ADR-010)

### Scale track (ADR-014: tens of TB, hundreds of millions of files)
- [ ] Synthetic scale generators (pm-testkit): NTFS volumes and file tables with 1 M / 10 M entries; `benches/` with divan; recorded budgets in docs/PERF.md
- [ ] NTFS bulk MFT enumeration (sequential chunked scan, parallel record parsing) feeding the loader
- [ ] Loader: deferred secondary-index creation, ≥ 200 k files/s, bounded memory (streaming parent-id resolution)
- [x] DB-backed views replace `pm-index`: tree, File Types, Deleted, Discovery, Gallery, Data Source Summary, Timeline via indexed SQL — done 2026-10-18; `pm-index` deleted
- [x] File-name/path search index in Tantivy (n-gram tokenizer) with sub-second substring search over 100 M names (shared `pm-search` crate with the M5 content index) — `pm_search::names` (positional trigrams, exact substring/prefix/name/path queries, keyset paging, `catch_up`); 10 M names: 4–25 ms per selective query (docs/PERF.md); UI hook pending (coordinator)
- [x] Timeline: events table + pre-aggregated per-unit counts; counts view never scans events — events table + time-column indexes; counts are indexed range histograms (no pre-aggregation needed so far)
- [ ] Huge directory UX: keyset-paged listings, cached child counts
- [ ] Memory/latency regression tests in CI for the 10 M-file synthetic case

### M2 — Case database & data sources
- [x] `pm-casedb` schema v1 (SQLite) + migrations + typed API (objects, files, VS/FS, data sources, artifacts/attributes with all standard types, tags, hosts/persons/OS accounts, accounts/relationships, events, ingest jobs, reports, scores) — schema v3
- [x] Standard artifact & attribute type catalogue (from inventory 01) as Rust enums + DB seed
- [x] Bulk loader: walker → channel → batched writer; benchmark (target ≥ 200 k files/s on SSD) — ~90 k rows/s measured on the VM (PERF.md)
- [x] Query API for UI: tree children, views (file types by extension/MIME, deleted, size buckets, recent), results by type, tags, keyset pagination, counts with caching
- [ ] `pm-case`: create/open/close/delete, `case.pmcase`, case properties, data source add/remove, event bus (in-process), settings store, logging setup — done except the event bus (ADR-023) and a case log file
- [ ] Golden-dump harness (`pm-testkit::golden`): normalised DB dump → insta snapshot
- [ ] Autopsy-case importer (stretch; SQLite → pm schema)

### M3 — UI shell and browsing (first usable release)
- [x] First visible slice (evidence-backed, pre-case-DB): eframe app, menus/toolbar/status bar, data-source tree (volumes, dirs, $OrphanFiles), virtualised sortable table with keyboard nav + context menu (pop out, extract, copy path), viewers Hex/Strings/Text/Application(picture)/File Metadata with auto tab choice, deferred-viewport pop-out windows, back/forward, drag-and-drop, themes, local/UTC time, persisted settings; `pm-testkit::demo` disk
- [x] eframe app skeleton, theming (light/dark), fonts, icons, settings persistence
- [ ] Main window: menu tree, toolbar, status bar, dock layout (`egui_dock`), reset windows — menus, toolbar, status bar done; no dock layout
- [x] Welcome window, recent cases, new-case wizard, open case, case properties
- [ ] Add-data-source wizard (all DSPs, time zone, ingest-module selection page, progress) — disk images only; time zone, module selection and progress done
- [x] Directory tree (data sources, views, results, tags, OS accounts, hosts/persons, reports, score) with counts, back/forward history, "view in directory"
- [ ] Result table: virtualised, sortable, column chooser, saved layouts, CSV export, thumbnails tab, summary tab, paging-free scrolling — virtualised, sortable, CSV export, result tabs done; column chooser, saved layouts, thumbnails tab pending
- [ ] Content viewers: hex (virtualised, go-to offset, select/copy), strings (scripts/encodings), text, image (zoom/rotate/EXIF), metadata, file layout, results/annotations, analysis results, OS account, context — hex, strings, text, extracted text, image, metadata done; results/annotations/analysis-results/OS-account/context viewers pending
- [x] Pop-out viewer windows (multi-viewport) for content, results and analysis views; "view in new window" for nodes
- [x] Context menus: extract/export files & folders (with hashing), open in external viewer, copy path, add/remove tags (with comments), mark notable, view in timeline — folders and multi-select pending
- [ ] Tag management, tag sets, options dialog (all categories), keyboard shortcuts, about — tags, tag sets, about done; options: View, Tags, Hash Sets; few shortcuts
- [x] File search by attributes dialog
- [x] UI tests with `egui_kittest` for the main flows

### M4 — Ingest framework and core modules
- [ ] `pm-ingest`: job settings, profiles, file filters, module factory API, staged pipelines (DS stage 1, file, artifact, result, DS stage 2), work-stealing scheduler with priorities, batched results, cancellation, progress snapshots, ingest inbox, ingest history in DB, run ingest on existing data sources — done except profiles and the artifact/result pipelines
- [x] Hash Lookup (+ `pm-hashdb`: all formats, indexing, known/notable/no-change, Central Repo hash sets) — except Central Repo hash sets
- [x] File Type Identification (`pm-filetype` + user-defined types)
- [x] Extension Mismatch Detector
- [x] Embedded File Extractor (zip, 7z, tar, gz/bz2/xz/zstd, rar if feasible, ISO, Office OLE/OOXML, PDF images; zip-bomb limits; password detection)
- [x] Picture Analyzer (EXIF → metadata + GPS artifacts, HEIC handling) — except HEIC
- [x] Encryption Detection (entropy, BitLocker/LUKS/FileVault/VeraCrypt signatures, password-protected Office/PDF/zip)
- [x] Interesting Files Identifier (full rule language, rule sets import/export)
- [x] Data Source Integrity (verify stored hashes, compute hashes)
- [ ] Ingest UI: module config panels, progress panel, inbox, snapshot/diagnostics — progress panel, inbox, module selection done; module config panels partial

### M5 — Text extraction and keyword search
- [x] `pm-text`: plain text with encoding detection, HTML, PDF, Office (OOXML, legacy OLE), RTF, SQLite, strings fallback, artifact text, language detection — done except language detection (gap, docs/agents/search.md)
- [x] `pm-search`: Tantivy schema, chunking (Autopsy parity), analyzers (CJK), exact/substring/regex, inline search during ingest, hit artifacts, highlighting
- [x] Keyword lists (built-ins with exact regexes, import/export incl. EnCase), credit-card validation (Luhn, BIN DB), account artifacts
- [x] Ad-hoc keyword search UI, extracted-text viewer with hit navigation
- [ ] OCR (pure-Rust `ocrs` or optional external Tesseract), machine translation provider interface

### M6 — Artifacts: Recent Activity and email
- [x] `pm-artifacts` registry hive parser (incl. transaction logs) + RegRipper-equivalent analyses (inventory 05 §2.7 list) — as `pm-registry`; see PARITY for the analyses
- [ ] Browsers: Chromium family, Firefox, Safari (incl. binarycookies), IE/Edge (WebCache ESE), Chrome cache — Chromium, Firefox, Safari, IE done; Edge legacy/WebCache (ESE) pending
- [ ] Windows: recycle bin, jump lists, LNK, prefetch (incl. MAM), SRUM, thumbcache, Zone.Identifier, OS info, user accounts, installed programs, USB devices (USB ID DB), shellbags, data source usage — all except SRUM and thumbcache
- [ ] Search-engine query analysis (SEUQA mappings), web account roles, domain categorisation
- [ ] `pm-email`: MBOX, EML/MIME, PST/OST, vCard, threading; attachments as derived files; email accounts and relationships — done except PST/OST
- [ ] Registry viewer, SQLite viewer, plist viewer, HTML viewer, PDF viewer, media viewer, message/contact/call-log artifact viewers

### M7 — Analysis views
- [x] Timeline (first cut): `pm-timeline` (event-type tree, calendar bins with Autopsy's RangeDivision rule, filters, counts, MACB list) + FS event collection + native-window UI (counts chart with drag/double-click/wheel zoom, list view, filters, history, presets, content viewer, View File in Timeline)
- [x] Timeline remaining: Details view (clustered stripes), artifact events (after M4/M6), tags/hash-hit filters, Add Event, Snapshot report, bottom range-slider histogram — Details, artifact events, range slider and DB-backed timeline done; tag/hash-hit filters in the UI, Add Event and Snapshot pending
- [x] Communications: accounts browser, relationship graph, message/contacts/call-log views, filters — graph partial
- [x] Geolocation: map tiles (online plus offline MBTiles), waypoints/tracks/routes/areas, filters, KML export — except offline MBTiles
- [x] Image Gallery (first cut): folder groups, background-decoded thumbnail grid, deleted marking, viewer + pop-out
- [ ] Image Gallery remaining: categories (Project VIC), tags, hash-hit marking, slideshow, persisted thumbnail cache, group sorting/seen state — now on the case DB with MD5 de-duplication
- [x] Data Source Summary (first cut): Types (pie + counts), Recent Files, Timeline (30-day activity + View in Timeline), Container
- [ ] Data Source Summary remaining tabs (User Activity, Analysis, Past Cases, Geolocation, Ingest History) after ingest
- [x] File Discovery (first cut): `pm-discovery` (Autopsy size buckets, data source/parent folder/deleted filters, group by size/folder/data source/extension, group and item ordering) + window with wizard, groups, paging, thumbnails
- [ ] Discovery remaining: domain discovery, past occurrences (CR), hash set/interesting item/object/keyword/tag filters, MD5 de-duplication

### M8 — Central repository
- [ ] `pm-centralrepo`: schema (SQLite and PostgreSQL), correlation types and normalisation, ingest module, notable flagging, Other Occurrences viewer, personas, organisations, Common Properties search, All Cases search, CR hash sets

### M9 — Reports
- [ ] Report wizard and module API; HTML, XLSX, CSV/TSV, KML, body file, portable case, CASE/UCO, tagged hashes, data source summary; snapshots via insta — wizard, HTML, XLSX, files text, KML, body file, tagged hashes done; portable case, CASE/UCO, report plugins pending

### M10 — Specialised modules
- [x] File carving (pure-Rust, PhotoRec-like signatures: `pm-carve` + PhotoRec Carver module)
- [ ] Android analyzer parsers (22 apps; inventory 03 §4) ported to Rust (table-driven query/mapping engine where it fits), GPX, DJI drones, VM extractor, YARA (yara-x), LEAPP-equivalent iOS/Android parsers or external aLEAPP/iLEAPP runner, Plaso runner (optional external), object detection (optional), memory-image DSP (Volatility runner, optional)

### M11 — Multi-user and automation
- [ ] PostgreSQL back end for `pm-casedb` (parity tests run against both back ends)
- [ ] `pm-server`: coordination locks, event bus, index service, collaboration monitor, health monitor
- [ ] Multi-user case browser, case locking semantics, remote events in the UI
- [ ] Auto-ingest: manifests, job queue, worker nodes, dashboards, file exporter, shared configuration
- [ ] `pm-cli`: command-line ingest (Autopsy-compatible options plus clap UX), reports, exports — `postmortem ingest` exists (create/open, add images, run modules); reports/exports/profiles pending
- [x] `pm-mcp`: MCP server with Autopsy's 17 tools (cross-platform)

### M12 — Plugins (ADR-013: Rust built-ins, WebAssembly plugins, no Python)
- [x] `pm-plugin-api`: WIT world (file access, metadata, artifact/result emission, settings, logging, progress), versioning policy
- [x] `pm-plugin-wasm`: wasmtime host (component model, fuel/memory limits, no ambient authority), plugin discovery/manifest, enable/disable UI, per-plugin settings
- [ ] Plugin kinds: ingest (file + data source), report, content viewer — ingest done; report and content-viewer kinds pending
- [x] Rust plugin template + scaffold command, example plugins, author docs; golden tests running plugins end-to-end — no scaffold command

### M13 — Hardening and release
- [ ] Benchmarks vs Autopsy-reported numbers; profiling passes
- [ ] Extended fuzz campaigns; golden regression suite over all fixtures
- [ ] Packaging (macOS app bundle, Windows MSI, Linux AppImage/deb), user docs

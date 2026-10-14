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

```
            ┌──────────────── postmortem (GUI bin) ── pm-cli (CLI bin) ── pm-server / pm-worker (bins)
            │                       │
          pm-ui (egui)          pm-app (application services: case session, jobs, events, settings)
            │                       │
   ┌────────┴──── analysis: pm-timeline pm-comms pm-geo pm-discovery pm-summary pm-centralrepo pm-report
   │            ingest:   pm-ingest  + modules (pm-mod-*)   search: pm-text pm-search   pm-email  pm-artifacts
   │            case:     pm-case  ── pm-casedb (SQLite | PostgreSQL)
   │            evidence: pm-evidence ── pm-image  pm-volume  pm-fs (+ pm-fs-*)
   └──────────  pm-core (ReadAt, decoding, time, units)          pm-testkit (fixture builders)
```

Dependency rules: arrows point down only. Parsers (`pm-image`, `pm-volume`,
`pm-fs-*`, `pm-artifacts`, `pm-email`, `pm-text`) know nothing about cases or
the database. Ingest modules depend on `pm-ingest` + parsers + `pm-casedb`.
`pm-ui` talks only to `pm-app` and read-side query APIs; it never runs
blocking IO on the UI thread.

### Data flow

1. **Add data source** (`pm-evidence`): open the image → detect the volume
   system → detect the filesystem per partition (or at offset 0) → walk the
   tree → bulk-insert `objects`/`files` rows in large transactions (100 k
   rows each). Unallocated space and orphan files become virtual files.
   Producer/consumer: walker threads → bounded channel → one DB writer.
2. **Ingest** (`pm-ingest`): a work-stealing (rayon) file pipeline plus
   data-source, artifact and result pipelines. Modules are `Send + Sync`
   factories that create per-thread instances. Results are buffered and
   flushed in batches by a single writer thread. Events fan out to the UI and,
   for multi-user cases, to the server.
3. **Browse** (`pm-ui`): tree and table read through paginated, indexed
   queries (keyset pagination); a content cache sits behind viewers;
   thumbnails come from an on-disk cache.

### Case storage (ADR-002)

- Case folder: `case.pmcase` (TOML metadata), `case.db` (SQLite WAL) or a
  PostgreSQL DSN, `index/` (Tantivy), `cache/` (thumbnails, extracted text),
  `export/`, `reports/`, `logs/`, `modules/` (module output).
- Core tables: `objects` (id, parent_id, kind, data_source_id), `files`
  (name, parent_path, meta fields, flags, sizes, times, hashes, MIME,
  extension, known status, location: fs+inode or derived/local path),
  `fs_info`, `vs_info`, `volumes`, `data_sources`, `artifact_types`,
  `attribute_types`, `artifacts` (data artifacts and analysis results, with
  score, conclusion, justification, configuration), `attributes` (typed value
  columns), `tags`/`tag_names`/`tag_sets`, `hosts`, `persons`, `os_accounts`
  (+ realms, instances, attributes), `accounts` and `account_relationships`
  (communications), `events` (timeline: event types, descriptions at three
  levels), `ingest_jobs` / `ingest_modules`, `reports`, `review_status`,
  `scores` (aggregate per object), `content_cache_meta`.
- Indexes are designed from the inventory's list of UI query patterns.
  Enums are stored as integers. Text search lives in Tantivy, not SQL.
- One storage trait with two back ends, SQLite (rusqlite, bundled) and
  PostgreSQL. Migrations are versioned SQL per back end and are tested on
  both.

### UI (ADR to come)

- eframe/egui with `egui_dock` for the main layout: tree | results | content.
  The content viewer, result tables, timeline, communications, geolocation,
  image gallery, discovery and the ingest inbox can each pop out into their
  own OS window (an egui immediate viewport, so state stays shared).
- Virtualised tables (`egui_extras`) with SQL-backed paging, multi-column
  sort, a column chooser and per-node-type saved layouts.
- Every long operation is a background job with progress and cancel,
  surfaced in a jobs panel.
- `egui_kittest` drives UI tests; screenshot tests are optional.

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
| `pm-core` | ReadAt sources, bounded decoding, time, units | done (M0) |
| `pm-testkit` | fixture builders (disks, FS images, artefacts) | started |
| `pm-image` | raw/split, EWF/EWF2, VMDK, VHD/VHDX, QCOW2, DMG, AFF4 | raw + EWF v1 done |
| `pm-volume` | MBR/EBR, GPT, APM, BSD disklabel, LVM, BitLocker/LUKS detection | MBR+GPT done |
| `pm-fs` | FileSystem trait, ExtentSource, walker | done |
| `pm-fs-fat` | FAT12/16/32 | done |
| `pm-fs-exfat` | exFAT | done |
| `pm-fs-ntfs` | NTFS incl. ADS, compression, $LogFile/$UsnJrnl access, slack | core done |
| `pm-fs-ext` | ext2/3/4 incl. extents, inline data, journal | done (journal TBD) |
| `pm-fs-hfs` | HFS+/HFSX incl. resource forks, compression | — |
| `pm-fs-apfs` | APFS (unencrypted; snapshots later) | — |
| `pm-fs-iso` | ISO9660 (+Joliet/Rock Ridge) and UDF | — |
| `pm-fs-xfs`, `pm-fs-btrfs`, `pm-fs-yaffs` | lower priority filesystems | — |
| `pm-evidence` | layered detection: image → VS → FS registry; logical files; local disk | — |
| `pm-casedb` | schema, migrations, SQLite & PostgreSQL back ends, typed query API | — |
| `pm-case` | case lifecycle, metadata, data source processors, event bus, settings | — |
| `pm-ingest` | ingest framework (jobs, pipelines, filters, profiles, inbox) | — |
| `pm-filetype` | pure-Rust MIME detection (signature DB + custom types) | — |
| `pm-hashdb` | hash set formats (NSRL RDS, EnCase, md5sum, HashKeeper, idx, sqlite) + lookup | — |
| `pm-text` | text extraction (plain/HTML/PDF/Office/RTF/SQLite/strings), encodings, language | — |
| `pm-search` | Tantivy indexing, chunking, keyword lists, exact/substring/regex search, hits | — |
| `pm-artifacts` | registry, EVTX, LNK, prefetch, jump lists, ESE/SRUM/WebCache, recycle bin, thumbcache, browsers, plist… | — |
| `pm-email` | MBOX/EML/PST/OST/vCard, threading | — |
| `pm-mod-*` | ingest modules (hash, filetype, extmismatch, embedded, picture, encryption, interesting, integrity, keyword, recent-activity, email, android, leapp, gpx, drones, vm, yara, carver, plaso, objectdetect, memory) | — |
| `pm-timeline` | event model, filters, zoom, aggregation queries | — |
| `pm-comms` | accounts, relationships, graph layout | — |
| `pm-geo` | waypoints/tracks/areas, KML | — |
| `pm-discovery` | file/domain discovery, grouping, ranking | — |
| `pm-summary` | data source summary computations | — |
| `pm-centralrepo` | correlation DB, normalisation, other occurrences, personas, common properties | — |
| `pm-report` | HTML, XLSX, CSV/TSV, KML, body file, portable case, CASE/UCO, tagged hashes | — |
| `pm-app` | application services used by UI/CLI/server | — |
| `pm-ui` | egui application (shell, tree, tables, viewers, wizards, options, analysis UIs) | — |
| `postmortem` | GUI binary | stub |
| `pm-cli` | command-line ingest/report/export binary | — |
| `pm-server`, `pm-worker` | multi-user server and auto-ingest worker | — |
| `pm-mcp` | MCP server exposing case queries | — |
| `pm-plugin-api`, `pm-plugin-wasm` | WebAssembly plugin system (ADR-013) | — |
| `fuzz/` | cargo-fuzz targets (outside the workspace) | — |

## Milestones

The order favours an early end-to-end vertical slice (open image → browse →
view) and then widens. Each milestone ends with: gate green, fuzz smoke run
for new parsers, benchmarks recorded, PROGRESS log entry.

### M0 — Foundations ✅
- [x] Workspace, toolchain pin, lints, loop protocol
- [x] `pm-core`: ReadAt sources, Bytes decoding, timestamps, units
- [x] `pm-volume`: MBR/EBR + GPT with TSK-style layout
- [x] `pm-image`: raw + split raw, format sniffing
- [x] `pm-fs`: trait, ExtentSource, walker
- [x] `pm-fs-fat`: FAT12/16/32 with deleted-file recovery

### M1 — Evidence layer (vertical-slice subset first)
- [x] `fuzz/` crate with targets for volume, FAT; smoke script `scripts/fuzz-smoke.sh` (seeds in `fuzz/seeds/`)
- [x] `pm-image` EWF v1 (E01/s01): segments (E01…ZZZ), zlib/stored chunks, >2 GiB offset overflow, stored MD5/SHA-1, header2/header metadata, error2; verified against ewfacquire (9 format variants, multi-segment) + fuzz target
- [ ] `pm-image` EWF2 (Ex01/Lx01) and L01 logical evidence (file tree in `ltree`)
- [x] `pm-fs-ntfs`: boot, MFT (incl. fragmented $MFT via attribute list), fixups (torn-tolerant), runlists, resident/non-resident, LZNT1, sparse, init size, ADS, $SI+$FN times, deleted via parent map, $I30 slack, orphans, unallocated clusters. In-house `NtfsBuilder` (pm-testkit) validated by TSK; differential vs `fls` + `ntfs` crate oracle; fuzz target
- [ ] NTFS extras: $Secure/owner SIDs, reparse points/junctions, $UsnJrnl/$LogFile parsing, EFS flagging, WOF/compact compression, hard-link enumeration, bulk MFT enumeration fast path
- [ ] `pm-evidence`: probe chain (image → VS → FS registry), unallocated-space virtual files, partitions with unknown FS, "no VS" fallback, logical files & folders DSP, local disk DSP (read-only)
- [x] `pm-fs-exfat`: entry sets with checksum validation, deleted sets, NoFatChain/valid-length, UTC offsets (TSK gets these wrong), alloc bitmap, orphan scan; macOS-generated fixture + fls differential + fuzz
- [x] `pm-fs-ext` (ext2/3/4): extents (any depth, uninit), indirect blocks, inline data, fast symlinks, dirent-slack deleted names, bitmaps incl. uninit groups, nanosecond/crtime times; fixtures via `fstool` (pinned dev-dep) + fls differential
- [ ] `pm-fs-hfs` (HFS+), `pm-fs-apfs`
- [ ] `pm-fs-iso` (ISO9660/Joliet/RR, UDF)
- [ ] `pm-image` VMDK (sparse + flat + descriptor), VHD (fixed/dynamic/diff), VHDX, QCOW2, DMG (UDIF)
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
- [ ] DB-backed views replace `pm-index`: tree, File Types, Deleted, Discovery, Gallery, Data Source Summary via indexed SQL + precomputed aggregate tables
- [ ] File-name search index (FTS5 trigram) with sub-second substring search over 100 M names
- [ ] Timeline: events table + pre-aggregated per-unit counts; counts view never scans events
- [ ] Huge directory UX: keyset-paged listings, cached child counts
- [ ] Memory/latency regression tests in CI for the 10 M-file synthetic case

### M2 — Case database & data sources
- [ ] `pm-casedb` schema v1 (SQLite) + migrations + typed API (objects, files, VS/FS, data sources, artifacts/attributes with all standard types, tags, hosts/persons/OS accounts, accounts/relationships, events, ingest jobs, reports, scores)
- [ ] Standard artifact & attribute type catalogue (from inventory 01) as Rust enums + DB seed
- [ ] Bulk loader: walker → channel → batched writer; benchmark (target ≥ 200 k files/s on SSD)
- [ ] Query API for UI: tree children, views (file types by extension/MIME, deleted, size buckets, recent), results by type, tags, keyset pagination, counts with caching
- [ ] `pm-case`: create/open/close/delete, `case.pmcase`, case properties, data source add/remove, event bus (in-process), settings store, logging setup
- [ ] Golden-dump harness (`pm-testkit::golden`): normalised DB dump → insta snapshot
- [ ] Autopsy-case importer (stretch; TSK SQLite → pm schema)

### M3 — UI shell and browsing (first usable release)
- [x] First visible slice (evidence-backed, pre-case-DB): eframe app, menus/toolbar/status bar, data-source tree (volumes, dirs, $OrphanFiles), virtualised sortable table with keyboard nav + context menu (pop out, extract, copy path), viewers Hex/Strings/Text/Application(picture)/File Metadata with auto tab choice, deferred-viewport pop-out windows, back/forward, drag-and-drop, themes, local/UTC time, persisted settings; `pm-testkit::demo` disk
- [ ] eframe app skeleton, theming (light/dark), fonts, icons, settings persistence
- [ ] Main window: menu tree, toolbar, status bar, dock layout (`egui_dock`), reset windows
- [ ] Welcome window, recent cases, new-case wizard, open case, case properties
- [ ] Add-data-source wizard (all DSPs, time zone, ingest-module selection page, progress)
- [ ] Directory tree (data sources, views, results, tags, OS accounts, hosts/persons, reports, score) with counts, back/forward history, "view in directory"
- [ ] Result table: virtualised, sortable, column chooser, saved layouts, CSV export, thumbnails tab, summary tab, paging-free scrolling
- [ ] Content viewers: hex (virtualised, go-to offset, select/copy), strings (scripts/encodings), text, image (zoom/rotate/EXIF), metadata, file layout, results/annotations, analysis results, OS account, context
- [ ] Pop-out viewer windows (multi-viewport) for content, results and analysis views; "view in new window" for nodes
- [ ] Context menus: extract/export files & folders (with hashing), open in external viewer, copy path, add/remove tags (with comments), mark notable, view in timeline
- [ ] Tag management, tag sets, options dialog (all categories), keyboard shortcuts, about
- [ ] File search by attributes dialog
- [ ] UI tests with `egui_kittest` for the main flows

### M4 — Ingest framework and core modules
- [ ] `pm-ingest`: job settings, profiles, file filters, module factory API, staged pipelines (DS stage 1, file, artifact, result, DS stage 2), work-stealing scheduler with priorities, batched results, cancellation, progress snapshots, ingest inbox, ingest history in DB, run ingest on existing data sources
- [ ] Hash Lookup (+ `pm-hashdb`: all formats, indexing, known/notable/no-change, Central Repo hash sets)
- [ ] File Type Identification (`pm-filetype` + user-defined types)
- [ ] Extension Mismatch Detector
- [ ] Embedded File Extractor (zip, 7z, tar, gz/bz2/xz/zstd, rar if feasible, ISO, Office OLE/OOXML, PDF images; zip-bomb limits; password detection)
- [ ] Picture Analyzer (EXIF → metadata + GPS artifacts, HEIC handling)
- [ ] Encryption Detection (entropy, BitLocker/LUKS/FileVault/VeraCrypt signatures, password-protected Office/PDF/zip)
- [ ] Interesting Files Identifier (full rule language, rule sets import/export)
- [ ] Data Source Integrity (verify stored hashes, compute hashes)
- [ ] Ingest UI: module config panels, progress panel, inbox, snapshot/diagnostics

### M5 — Text extraction and keyword search
- [ ] `pm-text`: plain text with encoding detection, HTML, PDF, Office (OOXML, legacy OLE), RTF, SQLite, strings fallback, artifact text, language detection
- [ ] `pm-search`: Tantivy schema, chunking (Autopsy parity), analyzers (CJK), exact/substring/regex, inline search during ingest, hit artifacts, highlighting
- [ ] Keyword lists (built-ins with exact regexes, import/export incl. EnCase), credit-card validation (Luhn, BIN DB), account artifacts
- [ ] Ad-hoc keyword search UI, extracted-text viewer with hit navigation
- [ ] OCR (pure-Rust `ocrs` or optional external Tesseract), machine translation provider interface

### M6 — Artifacts: Recent Activity and email
- [ ] `pm-artifacts` registry hive parser (incl. transaction logs) + RegRipper-equivalent analyses (inventory 05 §2.7 list)
- [ ] Browsers: Chromium family, Firefox, Safari (incl. binarycookies), IE/Edge (WebCache ESE), Chrome cache
- [ ] Windows: recycle bin, jump lists, LNK, prefetch (incl. MAM), SRUM, thumbcache, Zone.Identifier, OS info, user accounts, installed programs, USB devices (USB ID DB), shellbags, data source usage
- [ ] Search-engine query analysis (SEUQA mappings), web account roles, domain categorisation
- [ ] `pm-email`: MBOX, EML/MIME, PST/OST, vCard, threading; attachments as derived files; email accounts and relationships
- [ ] Registry viewer, SQLite viewer, plist viewer, HTML viewer, PDF viewer, media viewer, message/contact/call-log artifact viewers

### M7 — Analysis views
- [x] Timeline (first cut): `pm-timeline` (event-type tree, calendar bins with Autopsy's RangeDivision rule, filters, counts, MACB list) + FS event collection + native-window UI (counts chart with drag/double-click/wheel zoom, list view, filters, history, presets, content viewer, View File in Timeline)
- [ ] Timeline remaining: Details view (clustered stripes), artifact events (after M4/M6), tags/hash-hit filters, Add Event, Snapshot report, bottom range-slider histogram
- [ ] Communications: accounts browser, relationship graph, message/contacts/call-log views, filters
- [ ] Geolocation: map tiles (online plus offline MBTiles), waypoints/tracks/routes/areas, filters, KML export
- [x] Image Gallery (first cut): folder groups, background-decoded thumbnail grid, deleted marking, viewer + pop-out
- [ ] Image Gallery remaining: categories (Project VIC), tags, hash-hit marking, slideshow, persisted thumbnail cache, group sorting/seen state
- [x] Data Source Summary (first cut): Types (pie + counts), Recent Files, Timeline (30-day activity + View in Timeline), Container
- [ ] Data Source Summary remaining tabs (User Activity, Analysis, Past Cases, Geolocation, Ingest History) after ingest
- [x] File Discovery (first cut): `pm-discovery` (Autopsy size buckets, data source/parent folder/deleted filters, group by size/folder/data source/extension, group and item ordering) + window with wizard, groups, paging, thumbnails
- [ ] Discovery remaining: domain discovery, past occurrences (CR), hash set/interesting item/object/keyword/tag filters, MD5 de-duplication

### M8 — Central repository
- [ ] `pm-centralrepo`: schema (SQLite and PostgreSQL), correlation types and normalisation, ingest module, notable flagging, Other Occurrences viewer, personas, organisations, Common Properties search, All Cases search, CR hash sets

### M9 — Reports
- [ ] Report wizard and module API; HTML, XLSX, CSV/TSV, KML, body file, portable case, CASE/UCO, tagged hashes, data source summary; snapshots via insta

### M10 — Specialised modules
- [ ] Android analyzer parsers (22 apps; inventory 03 §4) ported to Rust (table-driven query/mapping engine where it fits), GPX, DJI drones, VM extractor, YARA (yara-x), file carving (pure-Rust, PhotoRec-like signatures), LEAPP-equivalent iOS/Android parsers or external aLEAPP/iLEAPP runner, Plaso runner (optional external), object detection (optional), memory-image DSP (Volatility runner, optional)

### M11 — Multi-user and automation
- [ ] PostgreSQL back end for `pm-casedb` (parity tests run against both back ends)
- [ ] `pm-server`: coordination locks, event bus, index service, collaboration monitor, health monitor
- [ ] Multi-user case browser, case locking semantics, remote events in the UI
- [ ] Auto-ingest: manifests, job queue, worker nodes, dashboards, file exporter, shared configuration
- [ ] `pm-cli`: command-line ingest (Autopsy-compatible options plus clap UX), reports, exports
- [ ] `pm-mcp`: MCP server with Autopsy's 17 tools (cross-platform)

### M12 — Plugins (ADR-013: Rust built-ins, WebAssembly plugins, no Python)
- [ ] `pm-plugin-api`: WIT world (file access, metadata, artifact/result emission, settings, logging, progress), versioning policy
- [ ] `pm-plugin-wasm`: wasmtime host (component model, fuel/memory limits, no ambient authority), plugin discovery/manifest, enable/disable UI, per-plugin settings
- [ ] Plugin kinds: ingest (file + data source), report, content viewer
- [ ] Rust plugin template + scaffold command, example plugins, author docs; golden tests running plugins end-to-end

### M13 — Hardening and release
- [ ] Benchmarks vs Autopsy-reported numbers; profiling passes
- [ ] Extended fuzz campaigns; golden regression suite over all fixtures
- [ ] Packaging (macOS app bundle, Windows MSI, Linux AppImage/deb), user docs

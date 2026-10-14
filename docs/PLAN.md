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

`pm-plugin-api` is a stable Rust trait set (ingest module, report module,
content viewer, data source processor). On top of it sit two hosts:
- WASM components (wasmtime, WIT interface): the sandboxed, recommended route.
- Python modules (pyo3, optional cargo feature): parity with Autopsy's
  Jython modules.

## Crate map

| Crate | Purpose | Status |
|-------|---------|--------|
| `pm-core` | ReadAt sources, bounded decoding, time, units | done (M0) |
| `pm-testkit` | fixture builders (disks, FS images, artefacts) | started |
| `pm-image` | raw/split, EWF/EWF2, VMDK, VHD/VHDX, QCOW2, DMG, AFF4 | raw done |
| `pm-volume` | MBR/EBR, GPT, APM, BSD disklabel, LVM, BitLocker/LUKS detection | MBR+GPT done |
| `pm-fs` | FileSystem trait, ExtentSource, walker | done |
| `pm-fs-fat` | FAT12/16/32 | done |
| `pm-fs-exfat` | exFAT | — |
| `pm-fs-ntfs` | NTFS incl. ADS, compression, $LogFile/$UsnJrnl access, slack | — |
| `pm-fs-ext` | ext2/3/4 incl. extents, inline data, journal | — |
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
| `pm-plugin-api`, `pm-plugin-wasm`, `pm-plugin-python` | plugin system | — |
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
- [ ] `pm-image` EWF (E01/Ex01 v1): segments, zlib chunks, table/table2, stored hashes, metadata; generate fixtures (in-house EWF writer in testkit)
- [ ] `pm-fs-ntfs`: boot, MFT, attribute lists, runlists, resident/non-resident, compressed (LZNT1), sparse, ADS, $FILE_NAME times, deleted entries, orphans, $I30 index + slack entries, $Secure/owner later, unallocated clusters; fixtures (testkit NTFS builder or hdiutil-free generation; document provenance)
- [ ] `pm-evidence`: probe chain (image → VS → FS registry), unallocated-space virtual files, partitions with unknown FS, "no VS" fallback, logical files & folders DSP, local disk DSP (read-only)
- [ ] `pm-fs-exfat`
- [ ] `pm-fs-ext` (ext2/3/4)
- [ ] `pm-fs-hfs` (HFS+), `pm-fs-apfs`
- [ ] `pm-fs-iso` (ISO9660/Joliet/RR, UDF)
- [ ] `pm-image` VMDK (sparse + flat + descriptor), VHD (fixed/dynamic/diff), VHDX, QCOW2, DMG (UDIF)
- [ ] `pm-volume` APM, BSD disklabel; detect-only for LVM, BitLocker, LUKS, Core Storage
- [ ] slack-space content for FAT/NTFS (Autopsy "-slack" files)
- [ ] XFS, Btrfs, YAFFS2 (lower priority; may move to M13)

### M2 — Case database & data sources
- [ ] `pm-casedb` schema v1 (SQLite) + migrations + typed API (objects, files, VS/FS, data sources, artifacts/attributes with all standard types, tags, hosts/persons/OS accounts, accounts/relationships, events, ingest jobs, reports, scores)
- [ ] Standard artifact & attribute type catalogue (from inventory 01) as Rust enums + DB seed
- [ ] Bulk loader: walker → channel → batched writer; benchmark (target ≥ 200 k files/s on SSD)
- [ ] Query API for UI: tree children, views (file types by extension/MIME, deleted, size buckets, recent), results by type, tags, keyset pagination, counts with caching
- [ ] `pm-case`: create/open/close/delete, `case.pmcase`, case properties, data source add/remove, event bus (in-process), settings store, logging setup
- [ ] Golden-dump harness (`pm-testkit::golden`): normalised DB dump → insta snapshot
- [ ] Autopsy-case importer (stretch; TSK SQLite → pm schema)

### M3 — UI shell and browsing (first usable release)
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
- [ ] Timeline: event generation, filters, counts/details/list views, zoom, histogram, snapshot report
- [ ] Communications: accounts browser, relationship graph, message/contacts/call-log views, filters
- [ ] Geolocation: map tiles (online plus offline MBTiles), waypoints/tracks/routes/areas, filters, KML export
- [ ] Image Gallery: grouping, categories (Project VIC), hash-hit marking, slideshow, thumbnail cache
- [ ] Discovery (file and domain), Data Source Summary (all tabs)

### M8 — Central repository
- [ ] `pm-centralrepo`: schema (SQLite and PostgreSQL), correlation types and normalisation, ingest module, notable flagging, Other Occurrences viewer, personas, organisations, Common Properties search, All Cases search, CR hash sets

### M9 — Reports
- [ ] Report wizard and module API; HTML, XLSX, CSV/TSV, KML, body file, portable case, CASE/UCO, tagged hashes, data source summary; snapshots via insta

### M10 — Specialised modules
- [ ] Android analyzer parsers (22 apps; inventory 03 §4), GPX, DJI drones, VM extractor, YARA (yara-x), file carving (pure-Rust, PhotoRec-like signatures), LEAPP-equivalent iOS/Android parsers or external aLEAPP/iLEAPP runner, Plaso runner (optional external), object detection (optional), memory-image DSP (Volatility runner, optional)

### M11 — Multi-user and automation
- [ ] PostgreSQL back end for `pm-casedb` (parity tests run against both back ends)
- [ ] `pm-server`: coordination locks, event bus, index service, collaboration monitor, health monitor
- [ ] Multi-user case browser, case locking semantics, remote events in the UI
- [ ] Auto-ingest: manifests, job queue, worker nodes, dashboards, file exporter, shared configuration
- [ ] `pm-cli`: command-line ingest (Autopsy-compatible options plus clap UX), reports, exports
- [ ] `pm-mcp`: MCP server with Autopsy's 17 tools (cross-platform)

### M12 — Plugins
- [ ] `pm-plugin-api` stable traits and data types; WIT world for WASM; wasmtime host; pyo3 host (feature); example plugins and docs

### M13 — Hardening and release
- [ ] Benchmarks vs Autopsy-reported numbers; profiling passes
- [ ] Extended fuzz campaigns; golden regression suite over all fixtures
- [ ] Packaging (macOS app bundle, Windows MSI, Linux AppImage/deb), user docs

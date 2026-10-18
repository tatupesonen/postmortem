# Progress

## Now — RESUME HERE (2026-10-18, Linux machine, solo)

Read `docs/DIRECTION.md` first (owner decisions and working rules).

Releases since 0.2.0 (2026-10-18; tags per feature, semver in
`DIRECTION.md`):
* 0.2.1: time-zone warning after Recent Activity (switch re-reads
  FAT/exFAT times), Windows local disks (sector-aligned device reads).
* 0.2.2: multi-select in file and result tables, keyword-index memory
  bound (peak heap 1.55 → 0.8 GB on the Greg image), UI task pool,
  rows without inodes no longer select together, `app.rs` split into
  `app/`, components own their change handling, `pm_casedb::query`.
* 0.2.3: command palette (Ctrl+K, `file:N` / `artifact:N`), keyboard
  shortcuts (Ctrl+E/F/1–9, J/K, 1–9 tags, Help › Keyboard Shortcuts).
* 0.2.5: Case Overview dashboard (default page), IOC Search, timeline
  context (±N around an item, collapsed repeats, exclude text), tree
  nodes clicked while still leaves open once they get children.
* 0.2.4: Pivot window, Notes with findings in the HTML report
  (schema v4), tree nodes keep their open state.

Since 0.2.6 (on master, untagged): Results and Annotations viewer tabs
(`pm-ui/viewers/related.rs`), Context viewer tab plus File Metadata's
"Downloaded From" row; the time-zone warning closes at once when kept.

IN PROGRESS, branch `sqlite-viewer-wip` (not on master): Autopsy's SQLite
content viewer, `pm-ui/src/viewers/sqlite.rs`, shown in the Application
tab for files starting `SQLite format 3\0` (`Sniffed::Sqlite`).
* Done there: temp copy with `-wal`/`-journal` companions (case siblings
  via `child_named`), opened by `pm_browser::sqlite::open`; table picker,
  100-row pages, "N entries", Export to CSV (rfd, background task), a
  column header's right-click "Display as" (Raw, Unix s/ms, WebKit µs,
  Mac absolute). `copy_out` moved to `model.rs`; pm-ui now depends on
  pm-browser, rusqlite and (non-dev) tempfile. Clippy clean; unit tests
  in `sqlite.rs` pass.
* To do: UI test `sqlite_viewer_pages_tables` (ui_flows.rs, `#[ignore]`d)
  times out because `click_exact("🖴 Browser")` on the logical-set tree
  node does not open its folder; find the right labels for the logical
  set's children (dump labels after the click), then un-ignore, run the
  pm-ui tests, merge to master, update PARITY.md §10 (SQLite viewer →
  Done) and tag a patch release.
* Then: Registry viewer (pm-registry exists), plist, PDF/HTML viewers.

Machine notes: heavy jobs run through `~/slop/heavy.sh` (own systemd
unit, memory capped); systemd-oomd killed the terminal scope otherwise.

Next:
* Analyst workflow (`BEYOND-AUTOPSY.md` §8): review state (seen marks,
  unseen counts), saved filters and views, screenshots in notes.
* ADR-023 item 5 (module API migration; needs plugin API additions: known
  status, data-source time zone, random-access reads), the Postgres
  dialect layer (ADR-024).
* `postmortem ingest --only <ids>`; EWF2 (Ex01/Lx01) reader.

## 2026-10-15: parallel parity push (goal: all Autopsy features, tested)

- Gate green on master (346 tests) after fmt/clippy fixes (`433a508`).
- **Parallel agent tracks** (each in its own git worktree, rules in
  `docs/agents/RULES.md`, status files `docs/agents/<track>.md`; the
  coordinator merges finished branches into master and runs the gate):
  `core-file-modules` (pm-filetype/File Type ID, Extension Mismatch,
  Encryption Detection, Interesting Files + FilesSet engine, Data Source
  Integrity), `derived-and-carving` (derived files + pm-ingest
  re-injection, Embedded File Extractor, PhotoRec-like carving),
  `browsers` (tests owed for IE/Chromium, Firefox, Safari, Edge ESE,
  Chrome cache/extensions, PSL), `windows-artifacts` (pm-registry +
  RegRipper analyses, LNK, jump lists, prefetch, recycle bin, SRUM),
  `search` (pm-text, Tantivy content + file-name search, Keyword Search
  module, credit cards), `email-mobile` (MBOX/EML/PST/vCard, accounts,
  Android analyzers, GPX, DJI, YARA, VM extractor), plus a read-only
  parity audit → `docs/PARITY.md`.
- Wave 2 (after audit/merges): Central Repository, Reports, other data
  source processors / image formats / file systems, UI integration of
  all new modules (pm-ui is coordinator-owned), remaining UI parity.

## Previous state (end of 2026-10-14) — historical snapshot, see the Log for later work

### What works (all on `master`)
- **Evidence & file systems:** raw/split/EWF images, MBR/GPT, NTFS (reference-exact,
  verified against `fls` on the NIST "Hacking Case" image), FAT, exFAT, ext.
- **Cases (`pm-case`, `pm-casedb`):** case folders (`case.pmcase` +
  `case.db`), parallel loader into SQLite (205 k rows/s synthetic),
  `$OrphanFiles`, `$Unalloc`, ADS rows, re-open, relocate, delete.
- **Ingest (`pm-ingest`, ADR-015):** priority scheduling, N workers, staged
  data-source modules, single writer, inbox, progress snapshots.
- **Modules (`pm-modules`):** Hash Lookup (+ `pm-hashdb`: kdb, NSRL v2/v3,
  md5sum, HashKeeper, EnCase, idx); Picture Analyzer (EXIF); **Recent
  Activity**: Internet Explorer (native `index.dat` history, Favorites,
  cookies), Chromium family (history, downloads, cookies, bookmarks,
  logins, autofill), web search queries (Autopsy SEUQA mappings).
- **UI (`pm-ui`):** case-first workflow (Welcome, New Case wizard,
  Open/Close Case), Add Data Source wizard with module + hash-set
  configuration, status-bar progress + Ingest Progress window, Data
  Artifacts / Analysis Results tree + result table, Views tree (by
  extension/MIME, deleted, size) from the case DB, file browsing,
  viewers, Gallery, Discovery, Data Source Summary.
- **Timeline:** Autopsy-style custom chart component
  (`pm-ui/src/timeline_chart.rs`: JavaFX-like stacked bar chart with
  axes/grid/ticks/legend/hover, plus the overview range slider), and
  **artifact events** (web history, downloads, cookies, bookmarks,
  searches, autofill, EXIF) loaded from the case DB
  (`pm-ui/src/timeline_artifacts.rs`); the timeline rebuilds when an
  analysis finishes. **Details view** (`pm-ui/src/timeline_details.rs`):
  events grouped by type + description (Short/Medium/Full detail),
  clustered in time (gap = span/80), packed into labelled stripes on a
  time axis; click selects (viewer + event table), double-click zooms,
  Ctrl/⌘+wheel zooms.

### How to try it
1. `cargo build --release -p postmortem && ./target/release/postmortem`
2. Case › New Case…; Case › Add Data Source… → e.g.
   `~/Downloads/4Dell Latitude CPi.E01` (E02 found automatically); keep
   Recent Activity / Hash Lookup / Picture Analyzer enabled → Finish.
3. Status bar shows progress (click for details); results under Data
   Artifacts; Tools › Timeline shows file and web events.

### Not verified / debt (usage was limited today)
- **The full gate (`scripts/gate.sh`) was NOT run** for the last commits
  (Recent Activity, Picture Analyzer merge, timeline chart/artifact
  events). Run it first tomorrow and fix any fmt/clippy/test failures.
- **Tests owed:** Recent Activity (IE index.dat incl. multiple hash tables
  and MSHist daily files, Favorites, cookies; Chromium databases; search
  queries; idempotent re-runs; diff IE history vs `pasco` on the Dell
  image), Picture Analyzer end-to-end, timeline chart widget (kittest
  screenshot + interactions), timeline artifact events (mapping, source
  file location), overview range slider.
- Timeline: Details view lacks Autopsy's cluster expand/collapse into
  sub-clusters, pinning and the bundled "same description" grouping;
  toolbar/filter-panel restyle to match Autopsy and tag/hash-hit filters
  not done. Tests owed for the Details view (clustering, description
  levels beyond the unit test, packing, interactions).
  Artifact events lose their own description column (shown in path).
- Recent Activity gaps: Firefox (helper `prtime` already in place),
  Safari, legacy Edge (ESE WebCacheV01.dat), Chrome cache/favicons/
  extensions/web-form addresses, full public-suffix list, download
  KAAMOS_PATH_ID / associated objects, other Recent Activity parts
  (registry, recent docs, programs, USB, …).
- Module agent's remaining plan: File Type Identification (`pm-filetype`,
  Tika parity), Extension Mismatch, Encryption Detection, Interesting
  Files, Data Source Integrity, Embedded File Extractor.

### Next, in order
1. Run the gate, fix, add the owed tests above.
2. Tantivy file-name search (`pm-search`, design below in this file).
3. Firefox + remaining Recent Activity; File Type Identification.
4. Timeline Details polish (expand/pin) and Autopsy-style toolbar/filters.

## Blockers

_None._

## Log

### 2026-10-18 — Field test, DB timeline, architecture review (solo)

* Ran a real case: DFIR Madness "Stolen Szechuan Sauce" DESKTOP-SDN1RPT
  through `postmortem ingest` (new headless CLI) and the MCP server;
  answers match the published key (BEYOND-AUTOPSY §8). Ingest 4 min,
  peak resident 3.25 GB.
* Found and fixed: the DB timeline held only file times — data artifacts
  now post Autopsy timeline events in their transaction (back-filled once for
  older cases); Windows 10 jump-list times read from the wrong offset.
* Timeline window, Image Gallery (MD5 de-duplication) now read the case
  DB; schema v3 indexes all file time columns.
* Examiner actions: Options › Hash Sets, Add File to Hash Set, Export
  Table to CSV, Open in External Viewer, Delete Case, Open Case/Log
  Folder. Communications Visualization Tool.
* Architecture review recorded as ADR-023; Postgres plan as ADR-024;
  owner decisions in `DIRECTION.md`. Docs verified against the code.

### 2026-10-17 — Tracks merged; reports, plugins, MCP, email, geolocation

Merged the search (pm-text, pm-search, Keyword Search), core file
modules (File Type ID, Extension Mismatch, Encryption Detection,
Interesting Files, Data Source Integrity), archives/carving (Embedded File
Extractor, PhotoRec Carver), browsers (pm-browser, Recent Activity web),
image formats (VHD, VHDX, VMDK, QCOW2) and MCP tracks. Added the
WebAssembly plugin system (ADR-022), reports (HTML, Excel, files text,
body file, tagged hashes, KML), Email Parser, GPX Parser and the
Geolocation window, result tabs, keyword search UI with the Keyword Hits
tree, Extracted Text viewer, tagging, hosts/persons, Options. Released
v0.1.1.

### 2026-10-17 — WP-0 schema v2
- Migration `0002_schema_v2.sql` + `pm-casedb` modules `scores`, `tags`,
  `hosts`, `os_accounts` (+ `windows` SID rules), `communications`,
  `events`. Autopsy semantics throughout (ADR-017); FS events virtual
  (ADR-018). Significance codes reordered to Autopsy ordinals; v1 cases
  migrate and get scores backfilled. Notable tags score instead of
  setting `known`.
- Tests: Autopsy `OsAccountTest` ported (13 cases + delete-host guard), score
  ordering/override/artifact targets, tag-set exclusivity and rescoring,
  events, communications normalisation/relationships/filters, v1→v2
  migration, default host per data source. Workspace: 481 tests.
- Environment: toolchain clippy component reinstalled; the reference tools and
  ewf-tools installed; three agent tracks relaunched.

- 2026-10-14: pm-case loader (205 k rows/s synthetic), ingest framework, Hash Lookup, case-first UI with status-bar progress, results tree; NTFS reference-exact on the NIST Hacking Case image.
### 2026-10-14 — Picture Analyzer (EXIF)
- `pm-modules::picture_analyzer` + `exif` (random-access TIFF/JPEG EXIF
  reader): `KAAMOS_METADATA_EXIF` (DATETIME_CREATED in the data source's
  zone, GEO_LATITUDE/LONGITUDE/ALTITUDE, DEVICE_MODEL/MAKE) and
  `KAAMOS_USER_CONTENT_SUSPECTED`, per Autopsy's `EXIFProcessor`. Unit tests
  for the reader (both byte orders, multiple APP1, corrupt offsets,
  proptest) and dates; **no end-to-end ingest test yet**. HEIC conversion
  not implemented. Falls back to a JPEG/TIFF/WAV signature check until File
  Type Identification lands.

### 2026-10-14 — File Discovery
- `pm-discovery` crate (14 tests, Autopsy's size-bucket boundaries) and the
  Discovery window (wizard → groups → paged thumbnail/list results →
  viewer/pop-out). Shared `thumbs::ThumbCache` between Gallery and
  Discovery. Workspace: 234 tests.

### 2026-10-14 — file index, Data Source Summary, Image Gallery
- `pm-index`: parallel file catalogue built automatically per data source
  (it stands in for the case DB's file table). Timeline now builds from it.
- Data Source Summary in the result pane (Types pie with Autopsy colours,
  Recent Files, 30-day activity + View in Timeline, Container).
- Image Gallery window: folder groups, lazily decoded thumbnails, deleted
  marking, viewer, double-click pop-out. UI tests with screenshots for
  summary and gallery. Workspace: 219 tests.

### 2026-10-14 — ext + Timeline
- `pm-fs-ext` (ext2/3/4) using `fstool` (pinned exact) as the fixture
  writer. Exact `fls` parity on ext2/3/4; wired into `pm-evidence`.
- `pm-timeline`: Autopsy event-type hierarchy (sub-type colours tuned for
  contrast), calendar binning (RangeDivision rule, DST-correct), AND
  filters (types, data sources, file categories, deleted, text), counts
  and MACB list rows. Parallel FS event collection with progress.
- Timeline window (native pop-out): counts chart (hover, click to select a
  bin, double-click or drag to zoom, wheel zoom), list view, history,
  presets, filters, content viewer, and "View File in Timeline" from the
  table. Demo disk now has realistic varied timestamps (a deletion burst
  on 2021-05-31).
- Process slip: one commit went in red because `gate.sh` was piped. Fixed
  by amending; LOOP.md now says never pipe the gate. Workspace: 208 tests.

### 2026-10-14 — logo + exFAT
- Brand: `assets/brand` SVG icon (disk platter with a stitched Y-incision)
  and wordmark, rendered with resvg (`cargo run -p pm-ui --example
  render_brand`). Used as the window icon, on the welcome screen and in
  About.
- `pm-fs-exfat`: fixture written by macOS's own exFAT driver
  (`scripts/fixtures/make-exfat.sh`, gzip-committed); matches `fls`
  exactly. Timestamps honour the UTC offset; Autopsy 4.15 is off by the offset
  twice. Fuzzed for 2 min (149k runs, clean). Workspace: 179 tests.

### 2026-10-14 — UI first slice
- `pm-evidence` (image → VS → FS probe chain) and `pm-ui` (eframe 0.36):
  tree/table/viewers, background tasks, pop-out windows (deferred
  viewports), extract file, back/forward, persisted prefs. `postmortem`
  binary takes image paths. Demo disk at `pm-testkit::demo`
  (`cargo run -p pm-testkit --example make_demo -- out.dd`).
- egui 0.36 lessons: labels are selectable by default and swallow row
  clicks (disable `selectable_labels` in tables). Resizable panels persist
  their *content* size, so fill the panel with `set_min_size`. Tasks must
  `request_repaint_of` their own viewport.
- `egui_kittest` UI flow tests (3) with optional screenshots
  (`PM_UI_SCREENSHOT_DIR`). Workspace: 169 tests.

### 2026-10-14 — NTFS
- `pm-fs-ntfs` in-house (ADR-009 evaluation: the `ntfs` crate lacks MFT
  attribute lists, torn-record tolerance, LZNT1 and slack recovery, so it is
  used as an oracle). `pm-testkit::ntfs::NtfsBuilder` writes complete NTFS
  3.1 volumes (system files, multi-level $I30 B-trees, ADS, fragmented and
  compressed data, deleted files in slack, unindexed or orphaned). Autopsy reads
  them correctly (fls/icat/istat).
- Tests: tree/contents/ADS/metadata/deleted/orphans/unallocated, a `fls`
  differential, an `ntfs`-crate differential, a 128-case corruption
  proptest and a slack-scanner unit test. Fuzzing found 1 overflow and 1
  unbounded scan (fixed). Regressions are replayed from
  `fuzz/regressions/`.
- `pm_core::cache::ShardedLru`. Workspace: 150 tests.

### 2026-10-14 — reference-tool differential harness, EWF
- All 8 inventories landed (01–08). ADRs 007–012 record the crate and
  licence decisions.
- Dev tools installed: cargo-fuzz, libewf (ewfacquire/ewfinfo), the reference tools
  4.15 (mmls/fls/istat/icat).
- `pm_testkit::reftools` differential harness: volume layouts match mmls
  (unallocated semantics, GPT names, a 256-entry MBR name table generated
  from Autopsy). FAT trees match fls on FAT12/16/32, including `$OrphanFiles`
  (new orphan scan of free clusters). Recovering full deleted LFN names
  goes beyond Autopsy.
- EWF v1 reader verified against ewfacquire output across 9 format,
  compression and chunk-size variants. Fuzzing found a section-chain
  overflow on day one (fixed). `scripts/gate.sh` added; commits are now
  chained behind it.
- Workspace: 126 tests; 3 fuzz targets clean at 60 s each.

### 2026-10-14 — pm-fs-fat + PLAN draft
- `pm-fs-fat`: BPB/geometry with MS cluster-count typing, FAT12/16/32 tables,
  chain following with loop guards, LFN incl. deleted (checksum brute-force),
  CP437 short names, case flags, volume-label entries, virtual $MBR/$FATn,
  Autopsy recovery heuristic, reused-deleted-dir detection, unallocated sectors.
  Tested against `fatfs`-generated images (FAT12/16/32): tree, content, LFN,
  deleted recovery, cluster reuse, fragmentation, deleted dirs, time zones,
  plus a 256-case corruption proptest. Workspace: 103 tests.
- docs/PLAN.md drafted from inventories 02-06.

### 2026-10-14 — pm-image (raw)
- `pm-image`: container sniffing (EWF/EWF2/VMDK/VHD/VHDX/QCOW/DMG magic),
  split-raw segment discovery (.000/.001 numeric of any width, .aa alpha,
  case preserved, .000 candidate prompt as in Autopsy), `ConcatSource` with
  O(log n) segment lookup and truncation-safe short reads. Non-raw formats
  return `Unsupported` for now. Workspace total: 72 tests.

### 2026-10-14 — pm-volume
- `pm-volume`: DOS/MBR (primary + EBR chains, loop guard, FS-boot-sector
  rejection), GPT (CRC-validated header and array, backup fallback, 512/4096
  sectors, 30+ type GUID names). Autopsy-style full layout with Meta and
  Unallocated slots. `pm-testkit::DiskBuilder` builds MBR/EBR/GPT disks.
- 16 integration tests + 3 proptests (random sector 0, random MBR/EBR,
  GPT bit flips) checking layout invariants. Workspace total: 50 tests.

### 2026-10-14 — bootstrap
- Workspace skeleton: virtual manifest, pinned toolchain 1.97.1, workspace
  lints (pedantic + no-panic restrictions), clippy.toml test allowances.
- `pm-core`: `ReadAt` (Mem/File/Slice/Zero sources, Read+Seek cursor),
  bounds-checked `Bytes` decoding, UTF-16/C-string helpers, timestamp
  converters (FILETIME, WebKit, HFS, Cocoa, Unix s/ms/ns, DOS), SI/IEC size
  formatting. 34 tests (unit + proptest), gate green.

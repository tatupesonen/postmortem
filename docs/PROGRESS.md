# Progress

## Now — state at end of 2026-10-14 (resume here)

### What works (all on `master`)
- **Evidence & file systems:** raw/split/EWF images, MBR/GPT, NTFS (TSK-exact,
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
  analysis finishes.

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
- Timeline: only Counts and List views; Autopsy's **Details** view
  (clustered event stripes), toolbar/filter-panel restyle to match
  Autopsy, Description-detail zoom, tag/hash-hit filters not done.
  Artifact events lose their own description column (shown in path).
- Recent Activity gaps: Firefox (helper `prtime` already in place),
  Safari, legacy Edge (ESE WebCacheV01.dat), Chrome cache/favicons/
  extensions/web-form addresses, full public-suffix list, download
  TSK_PATH_ID / associated objects, other Recent Activity parts
  (registry, recent docs, programs, USB, …).
- Module agent's remaining plan: File Type Identification (`pm-filetype`,
  Tika parity), Extension Mismatch, Encryption Detection, Interesting
  Files, Data Source Integrity, Embedded File Extractor.

### Next, in order
1. Run the gate, fix, add the owed tests above.
2. Tantivy file-name search (`pm-search`, design below in this file).
3. Firefox + remaining Recent Activity; File Type Identification.
4. Timeline Details view and Autopsy-style toolbar/filters.

## Blockers

_None._

## Log

- 2026-10-14: pm-case loader (205 k rows/s synthetic), ingest framework, Hash Lookup, case-first UI with status-bar progress, results tree; NTFS TSK-exact on the NIST Hacking Case image.
### 2026-10-14 — Picture Analyzer (EXIF)
- `pm-modules::picture_analyzer` + `exif` (random-access TIFF/JPEG EXIF
  reader): `TSK_METADATA_EXIF` (DATETIME_CREATED in the data source's
  zone, GEO_LATITUDE/LONGITUDE/ALTITUDE, DEVICE_MODEL/MAKE) and
  `TSK_USER_CONTENT_SUSPECTED`, per Autopsy's `EXIFProcessor`. Unit tests
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
  exactly. Timestamps honour the UTC offset; TSK 4.15 is off by the offset
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
  compressed data, deleted files in slack, unindexed or orphaned). TSK reads
  them correctly (fls/icat/istat).
- Tests: tree/contents/ADS/metadata/deleted/orphans/unallocated, a `fls`
  differential, an `ntfs`-crate differential, a 128-case corruption
  proptest and a slack-scanner unit test. Fuzzing found 1 overflow and 1
  unbounded scan (fixed). Regressions are replayed from
  `fuzz/regressions/`.
- `pm_core::cache::ShardedLru`. Workspace: 150 tests.

### 2026-10-14 — TSK differential harness, EWF
- All 8 inventories landed (01–08). ADRs 007–012 record the crate and
  licence decisions.
- Dev tools installed: cargo-fuzz, libewf (ewfacquire/ewfinfo), sleuthkit
  4.15 (mmls/fls/istat/icat).
- `pm_testkit::tsk` differential harness: volume layouts match mmls
  (unallocated semantics, GPT names, a 256-entry MBR name table generated
  from TSK). FAT trees match fls on FAT12/16/32, including `$OrphanFiles`
  (new orphan scan of free clusters). Recovering full deleted LFN names
  goes beyond TSK.
- EWF v1 reader verified against ewfacquire output across 9 format,
  compression and chunk-size variants. Fuzzing found a section-chain
  overflow on day one (fixed). `scripts/gate.sh` added; commits are now
  chained behind it.
- Workspace: 126 tests; 3 fuzz targets clean at 60 s each.

### 2026-10-14 — pm-fs-fat + PLAN draft
- `pm-fs-fat`: BPB/geometry with MS cluster-count typing, FAT12/16/32 tables,
  chain following with loop guards, LFN incl. deleted (checksum brute-force),
  CP437 short names, case flags, volume-label entries, virtual $MBR/$FATn,
  TSK recovery heuristic, reused-deleted-dir detection, unallocated sectors.
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
  sectors, 30+ type GUID names). TSK-style full layout with Meta and
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

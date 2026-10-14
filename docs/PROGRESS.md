# Progress

## Now

- User asked (2026-10-14) for the analysis tools, starting with the Timeline
  (first cut done). Next, analysis tools that the file index supports
  without ingest: **Data Source Summary** (Types tab: counts by
  category/extension/deleted/size), **Image Gallery** (pictures grouped by
  folder, with thumbnails), **File Discovery** (filters by
  type/size/time/path). Then the remaining Timeline items, then M2 (case DB)
  so analysis results persist.
- M1 leftovers: HFS+/APFS, ISO, XFS (fstool can write fixtures for most),
  VMDK/VHD/QCOW2 (qemu-img).

## Blockers

_None._

## Log

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

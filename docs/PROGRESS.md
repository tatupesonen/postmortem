# Progress

## Now

- M1: `pm-fs-ntfs` is next. NTFS fixtures: there is no mkntfs; options are
  (a) `brew install ntfs-3g` for mkntfs plus a FUSE-free population route,
  (b) a Docker image, (c) an in-house NTFS writer in pm-testkit for unit
  tests and the `ntfs`/`mft` crates as dev-only oracles (ADR-009). Prefer (c)
  plus TSK `fls`/`istat`/`icat` differential tests on any generated image.
- Then `pm-evidence` (probe chain image → VS → FS) so M2 can start.

## Blockers

_None._

## Log

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

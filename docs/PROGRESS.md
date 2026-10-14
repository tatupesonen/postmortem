# Progress

## Now

- M1: set up `fuzz/` (cargo-fuzz, nightly) with targets for pm-volume and
  pm-fs-fat, plus `scripts/fuzz-smoke.sh`. Then EWF in `pm-image`, then
  `pm-fs-ntfs`, then `pm-evidence`.
- PLAN.md is a first draft. When inventories 01 (case/data model), 07 (Rust
  ecosystem) and 08 (Autopsy tests) land, refine the M2 schema tasks, pin
  crate choices (ADR) and add test-parity tasks.

## Blockers

_None._

## Log

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

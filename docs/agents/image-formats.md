# Track: image-formats (WP-6)

Virtual machine disk containers in `pm-image`, written in-house in pure
Rust (no libvmdk/libvhdi/libqcow bindings, ADR-009 table in
`docs/inventory/07-rust-ecosystem.md`).

## What is done

| Format | Subformats read | Parents |
|--------|-----------------|---------|
| VHD (`vhd.rs`) | fixed, dynamic, differencing; 511-byte legacy footer, footer copy at offset 0 as fallback | `W2ru`/`Wi2r` relative locator, `W2ku`/`Wi2k`/Mac locators, parent unicode name; unique-ID mismatch warned |
| VHDX (`vhdx.rs`) | dynamic, fixed, differencing; 512 and 4096-byte logical sectors; all payload states (NOT_PRESENT, UNDEFINED, ZERO, UNMAPPED, FULLY/PARTIALLY_PRESENT) with sector bitmaps | `relative_path`, `absolute_win32_path`, `volume_path` locator keys |
| VMDK (`vmdk.rs`) | descriptor files; monolithicSparse; twoGbMaxExtentSparse; monolithicFlat; twoGbMaxExtentFlat; streamOptimized (deflate grains and markers, footer GD); ZERO extents; zeroed-grain GTEs; redundant GD fallback; renamed monolithic files | `parentFileNameHint`; parent CID mismatch warned |
| QCOW2 (`qcow2.rs`) | v2 and v3; L1/L2; zero clusters; deflate and **zstd** compressed clusters; extended L2 (subclusters); lazy refcounts / dirty flag | backing file (any format, including raw) |

Shared machinery:
* `mapped.rs`: `BlockMap` trait (virtual offset → storage run) plus
  `MappedSource`, a single `ReadAt` used by all four formats. It streams data
  straight from the image files and coalesces physically contiguous
  grains/clusters into one read.
* `cache.rs`: `MetaPages`, a page cache (4 KiB pages, 1024 pages = 4 MiB per
  image) for every on-disk table (BAT, GD/GT, L1/L2, sector bitmaps).
  Tables are never loaded whole, so memory does not scale with image size.
  Decompressed grains/clusters use a separate LRU (4 MiB VMDK, 8 MiB QCOW2).
* `parent.rs`: parent resolution relative to the child's directory, then as
  stored (absolute), then by bare file name next to the child; chains deeper
  than 16 (or circular) are refused. A missing parent is
  `ImageError::MissingParent`, naming the child and the stored reference.
* `exts.rs`: Autopsy's `GeneralFilter` lists (`RAW_IMAGE_EXTS`,
  `ENCASE_IMAGE_EXTS`, `VIRTUAL_MACHINE_EXTS`), `SUPPORTED_IMAGE_EXTS`
  (adds `.qcow2`/`.qcow`), and `is_virtual_machine_file()` (the VM
  Extractor's selection rule).
* `open()` now dispatches every sniffed format; `open_paths()` (used when a
  case reopens a data source) sniffs the first path, so VM images and E01s
  reopen correctly (before, it always concatenated as raw). `open_raw()` is
  the old explicit-raw behaviour.
* Encrypted QCOW2 (AES or LUKS, header field or crypto extension) is refused
  with `ImageError::Encrypted`. External data files and QCOW v1 are refused.
* VHDX with a non-empty log: opened, `tracing::warn!`, and a `Warning`
  metadata entry ("log not replayed …") stored with the data source.

## Tests

* `tests/vm_qemu.rs` (35): `qemu-img` builds every subformat from a
  generated 12 MiB raw disk with sparse holes, single-sector islands and runs
  crossing block boundaries. Every byte is compared sequentially (odd 100 003
  byte reads) and with 400 random-offset/length reads. Also covered:
  backing chains made with `convert -B` (qcow2 over qcow2/raw, v2, VMDK
  delta) plus a missing-parent error after moving the base; 4.5 GiB split
  sparse/flat VMDKs (reads across the 2 GiB extent boundaries); encrypted
  LUKS qcow2 refusal; sniffing; and **TSK `img_cat` (libvhdi/libvmdk) as a
  second oracle** for VHD, monolithic/split sparse and streamOptimized VMDK.
  Skips with a message when `qemu-img` (or `img_cat`) is absent.
* `tests/vm_handbuilt.rs` (15): `pm-testkit::vdisk` builders for what qemu
  cannot write: differencing VHD (partial bitmaps, garbage in absent
  sectors, relative and absolute-locator fallback, missing parent), VHDX
  dynamic at 512/4096 sector sizes and 1/4/32 MiB blocks, dirty log,
  differencing VHDX with sector bitmaps, VMDK sparse + delta link, renamed
  monolithic VMDK, descriptor with missing extent / FLAT + ZERO extents,
  circular parents; corrupt headers for all four formats; proptest "never
  panics" on mutated valid images and on arbitrary bytes behind each magic.
* Unit tests: descriptor parsing, parent resolution, CRC-32C, LRU bounds,
  page cache edges, extension lists.
* `pm-case/tests/vm_images.rs`: end to end. The pm-testkit demo disk is
  wrapped as VHD, VHDX and VMDK (and, with qemu-img, streamOptimized VMDK,
  split VMDK and QCOW2), each is added to a case with `add_image`. The file
  listing must equal the raw demo disk's, and after reopening the case the
  file content must read back through the container.
* Fuzz: `fuzz/fuzz_targets/{vhd,vhdx,vmdk,qcow2}_read.rs` with seeds in
  `fuzz/seeds/<target>/` (qemu-made images plus tiny hand-made VHD/VHDX).

## Benchmarks

See `docs/PERF.md` ("Virtual disk formats"). Run with
`cargo test --release -p pm-image --test vm_bench -- --ignored --nocapture`.

## Gaps vs TSK / libvmdk / libvhdi / Autopsy

* TSK (and so Autopsy) opens VMDK and VHD only, and does **not** follow
  differencing/delta parents. postmortem follows parents for all formats
  and also reads VHDX and QCOW2 (Autopsy cannot).
* Not supported: VMDK `COWD`/vmfsSparse (ESX snapshot deltas),
  `SESPARSE`, `VMFSRDM`/`VMFSRAW` raw device maps (these give a clear
  "extent type … is not supported" error); QCOW v1; QCOW2 external data
  files and internal snapshots (the current state is read); VHDX log
  replay (flagged only); DMG, AFF4, EWF2 (still `Unsupported`).
* Parent identity is checked only as a warning (VHD unique ID, VMDK CID);
  VHDX `parent_linkage` is not compared with the parent's data write GUID.
* No differencing VHD/VHDX fixtures from real Hyper-V/Virtual PC: the
  differencing layout is tested with in-house builders (qemu-img cannot
  write them). The VHDX sector bitmap bit order (LSB first) follows the
  specification (MS-VHDX) and is not cross-checked against real images.
* UI: the Add Data Source file chooser filter lives in `pm-ui` (not edited
  by this track); it should use `pm_image::SUPPORTED_IMAGE_EXTS`. The VM
  Extractor (email-mobile track) should use `is_virtual_machine_file()` and
  `pm_image::open`.
* Autopsy's own test images (`CommonFiles_img*_v1.vhd`, `c*ds*_v1.vhd`,
  `*Detection_img1_v1.vhd`, …) are VHDs. They now open, but are not on this
  machine.

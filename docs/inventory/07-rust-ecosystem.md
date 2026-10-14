# 07 — Rust crate ecosystem survey for postmortem

Survey date: **2026-10-14**. Versions were checked the same day against the crates.io API
(`max_stable_version`, the licence of that version, its publish date, and total and 90-day downloads), and
key ones again with `cargo info`. Dependency edges come from `/api/v1/crates/<c>/<v>/dependencies`. egui API
details come from the published `egui-0.36.2` / `egui_extras-0.36.2` sources. Workspace constraints come from
`postmortem/Cargo.toml` and `docs/DECISIONS.md`: **Apache-2.0**, toolchain **1.97.1**, `unsafe_code = forbid`
in our crates, and ADR-001 "pure-Rust evidence parsing, no libtsk/libewf FFI". Sizes use SI units
(ADR-005).

**Legend**
- **Pure** — ✅ pure Rust · ⚠️ pure Rust by default with optional C/asm · ❌ needs C/C++ (FFI or `-sys`) or an external binary.
- **Maint.** — date of the latest stable release / total downloads (90-day downloads). Each crate's `rust-version` is ≤ 1.96, so all fit our 1.97 toolchain.
- **Verdict** — **USE**, **USE (pin)**, **EVAL** (promising, needs a hands-on audit first), **ORACLE** (dev/test-only cross-check), **AVOID**, or **IN-HOUSE**.

---

## 0. Cross-cutting findings (read first)

1. **The 2026 forensic crate landscape is broad but young.** Since about March 2026 several single-author
   families have published dozens of pure-Rust DFIR crates. Most have 0–20 GitHub stars, under 20 000
   downloads, and many appear to be AI-assisted:
   - **SecurityRonin / `h4x0r`**: `ewf`, `aff4`, `vmdk-core`, `vhdx-core`, `vhd-core`, `qcow2-core`,
     `dmg-core`, `ntfs-core`, `apfs-core`, `hfsplus-core`, `xfs-core`, `btrfs-core`, `bitlocker-core`,
     `luks-core`, `ese-core`, `prefetch-core`, `xpress-huffman`, `lnk-core`, … Apache-2.0, created
     June–August 2026, each paired with a `*-forensic` auditor crate.
   - **Sootmark** (`sootmark-*`): created September–October 2026, fewer than 1 000 downloads each.
   - **antimatter-studios** (`rust-fs-*`, `rust-img-*`): created 2026-10-06, under 500 downloads.
   - **Dil4rd/dpp** (`udif`, `hfsplus`, `apfs`), MIT.
   - **hxyulin/hadris** (`hadris-iso/-fat/-udf/-part/-apfs`), MIT, the most active and broadest of these.

   They are useful as **references and differential oracles**. Putting them on the evidence path would be
   unvetted supply chain. ADR-001 already calls for our own parsers, and this survey supports that.
2. **`keramics` (Joachim Metz, the libyal/dfVFS author)** is the most credible pure-Rust format library.
   It is Apache-2.0, on crates.io as `keramics-*` 0.0.1, and its README says "experimental, pre-release".
   Its `keramics-formats/src` already covers: `ewf`, `splitraw`, `vhd`, `vhdx`, `vmdk`, `qcow`, `vdi`, `pdi`,
   `udif`, `sparseimage`, `sparsebundle`, `mbr`, `gpt`, `apm`, `bsdlabel`, `sgilabel`, `linuxlvm`, `bde`
   (BitLocker), `luksde`, `cdsaencr` (CoreStorage), `volsnap` (VSS), `ntfs`, `fat`, `exfat`, `ext`, `hfs`,
   `apfs`, `xfs`, `decmpfs`, `plist`, and `cpio`. **Treat it as the primary reference implementation and an
   oracle. Re-evaluate adopting it each quarter.** Its API is unstable.
3. **SQLite native-library conflict.** `libsqlite3-sys` uses `links = "sqlite3"`, so only one version can
   be in the dependency graph.
   - `rusqlite 0.40.2` needs `libsqlite3-sys ^0.38`.
   - `sqlx-sqlite 0.9.0` needs `>=0.30.1, <0.38`.
   - `sea-query-rusqlite 0.8.0` still needs `rusqlite ^0.38`.

   **You cannot combine current rusqlite with sqlx's SQLite driver or with sea-query-rusqlite.** This drives
   the recommendation in §14.
4. **`yara-x 1.21.0` pins `wasmtime ^45.0.3`.** The latest wasmtime is 49.0.2. If the WASM plugin host uses
   wasmtime 49, the binary contains **two wasmtime copies**: more compile time and roughly 10–20 MB more
   binary. Either pin the plugin host to wasmtime 45.x or accept the duplication (§12, §17).
5. **Licence watch-list for an Apache-2.0 product.** Do not link these into the shipped binary:
   - GPL: `nt-hive` (GPL-2.0+), `ffuzzy` (GPL-2.0+), `ssdeep` (GPL-3.0+), `exhume_*` (GPL-3.0).
   - AGPL: `vshadow`, `heic` (imazen, AGPL or commercial), `mupdf`, `exif-oxide`.
   - LGPL: `libesedb`, `binary-cookies`, and libheif via `libheif-rs`.
   - Pick the BSD-2 side of `pure-magic`/`magic-db` (GPL-3.0 OR BSD-2).
   - MPL-2.0 (`symphonia`, `videoson`) is file-level copyleft and fine unmodified.
   - `tree_magic_mini`'s `with-gpl-data` feature pulls in GPL data.
6. **Pre-releases seen but not recommended:**
   - `pyo3 0.30.0`: in the changelog dated 2026-10-13, not yet on crates.io.
   - `tantivy 0.27.0`: in the changelog, not published.
   - `wasmtime 50.0.0-rc.1`, `winit 0.31.0-beta.3`, `notify 9.0.0-rc.5`, `dashmap 7.0.0-rc2`,
     `hadris-* 3.0.0-rc.1`, `smallvec 2.0.0-beta.2`, `zerocopy 0.9.0-alpha.0`.
   - **egui 0.37.0** is "UNRELEASED" on `main`. It brings colour emoji and a theme `StyleProvider`, and makes
     `default_fonts` non-default, which will need `features = ["default_fonts"]`.

---

## 1. GUI

| Need | Crate | Version | Licence | Pure | Maint. | Verdict / caveats |
|---|---|---|---|---|---|---|
| Framework | `egui` / `eframe` | **0.36.2** | MIT OR Apache-2.0 | ✅ (wgpu or glow) | 2026-09-08 · 26 M (6.4 M) | **USE.** MSRV 1.95. 0.36 added drag-to-open panels and window-chrome theme sync. 0.35 added `egui_inspection` + `egui_mcp`, `Classes`, and better IME. 0.34 switched to `skrifa`+`vello_cpu` font rendering. |
| Renderer | `egui-wgpu` (wgpu **30.0.1**) / `egui_glow` | 0.36.2 | MIT OR Apache-2.0 | ✅ | 2026-09-08 | **USE wgpu** (Metal on macOS). Caveat [#8361](https://github.com/emilk/egui/issues/8361): panic when a window exceeds `max_texture_dimension_2d` (8192). Large evidence images must be **tiled** into ≤ 8192 px textures. |
| Tables | `egui_extras::TableBuilder` | 0.36.2 | MIT OR Apache-2.0 | ✅ | 2026-09-08 | **USE** for small and medium tables. `body.rows(h, n, \|row\|…)` lays out only visible rows (true virtualisation). `heterogeneous_rows` iterates **every** row height, so it is O(n) per frame. Also has `scroll_to_row`, `sense`, `striped`, `resizable`, and `Column::{auto,remainder,exact}`. There is no sorting or filtering; we build those. |
| Large tables (≥ 10⁶ rows) | `egui_table` (rerun) | **0.10.0** | MIT OR Apache-2.0 | ✅ | 2026-08-05 · 1.5 M | **USE** for the file/result tables. Delegate-based with sticky columns and header groups. The delegate learns the visible row range before drawing, which fits paged `LIMIT/OFFSET`/keyset DB fetches. `egui_deferred_table` 0.4.0 is an alternative; it is small. |
| Docking / tabs | `egui_dock` | **0.21.1** | MIT | ✅ | 2026-08-06 · 4.8 M | **USE.** Repo moved to `anhosh/egui_dock`. 0.21 made `TabViewer::id()` mandatory. Tear-out goes into an **`egui::Window` inside the same viewport, not a native OS window.** Native pop-out is our code: remove the tab from `DockState`, show it with `show_viewport_deferred`, and re-dock it on `close_requested`. |
| Docking (alt.) | `egui_tiles` (rerun) | **0.17.1** | MIT OR Apache-2.0 | ✅ | 2026-08-18 · 2.4 M | EVAL. Supports n-ary splits and grids, which egui_dock lacks. Used by the Rerun viewer. |
| UI testing | `egui_kittest` (+ `kittest` **0.5.0**) | **0.36.2** | MIT OR Apache-2.0 | ✅ | 2026-09-08 · 1.1 M | **USE.** `Harness::new_ui/new/builder`, `run()`, AccessKit queries such as `get_by_label`, and `.click()`. Features `snapshot` + `wgpu` enable image snapshots in `tests/snapshots`. `UPDATE_SNAPSHOTS=true\|force`. `kittest.toml` sets `threshold` (default 0.6) and `max_failed_pixels` (default 0), per OS. GPU and OS differences mean snapshots should be generated on one CI OS. Test pop-out *content* by rendering the tab body in a harness. Native multi-window behaviour needs manual or `egui_mcp` checks. |
| Agent / remote inspection | `egui_inspection` 0.36.2, `egui_mcp` **0.2.0** | — | MIT OR Apache-2.0 | ✅ | 2026-09 | Optional dev tool. `EGUI_INSPECTION=1` exposes the AccessKit tree on port 5719. Useful for automated end-to-end checks. |
| Plots / timeline | `egui_plot` | **0.37.0** (for egui 0.36) | MIT OR Apache-2.0 | ✅ | 2026-08-05 · 8.7 M | **USE** for histograms and timeline density. Version numbers are offset by one from egui (0.37 ↔ egui 0.36). |
| Map tiles | `walkers` | **0.61.0** | MIT | ✅ | 2026-10-10 · 1.2 M | **USE.** Slippy-map widget with HTTP tiles. The `pmtiles` feature (`pmtiles` 0.24.1) gives **offline maps for air-gapped labs**. `mvt` adds vector tiles. Depends on `egui_extras ^0.36`. |
| Communications graph | `egui_graphs` (+ `petgraph` **0.8.3**) | **0.32.0** | MIT | ✅ | 2026-08-31 · 1.3 M | **USE.** Interactive petgraph view with force-directed layout, selection, and zoom/pan. For 10⁴+ nodes, pre-compute the layout off-thread and cluster. |
| Node editor (not a graph view) | `egui-snarl` | **0.12.0** | MIT OR Apache-2.0 | ✅ | 2026-08-26 · 58 k | Only if we build a visual ingest-pipeline editor. `egui_node_graph2` (2024) and `egui-graph-edit` (egui 0.35) lag behind. `egui_graph` needs egui 0.34. **AVOID** those three. |
| Image loading | `egui_extras` loaders + `image` **0.25.10** | — | MIT OR Apache-2.0 | ✅ (avif decode needs `avif-native`→dav1d ❌) | 2026-03-10 · 213 M | **USE `image` directly; do not use the `file://` loader for evidence.** Write a custom `egui::load::BytesLoader`/`ImageLoader` for `pm://object/<id>` URIs. Decode on a rayon worker into `ColorImage`, then `ctx.load_texture`. Cap pixel counts to guard against decompression bombs. Built-in loaders: file, http, image, svg (`resvg` 0.45 pinned inside), gif, and webp. |
| Icons | `egui-phosphor` | 0.14.0 | MIT OR Apache-2.0 | ✅ | 2026-09-10 | USE. |
| File dialogs | `rfd` | 0.17.2 | MIT | ❌ (native OS dialogs) | 2026-01-12 · 35 M | USE. `egui-file-dialog` 0.15.0 is a pure-egui alternative. |
| Markdown (help, reports) | `egui_commonmark` | 0.25.0 | MIT OR Apache-2.0 | ✅ | 2026-08-05 | Optional. |
| Audio playback | `symphonia` **0.6.1** + `rodio` **0.23.0** / `cpal` **0.18.2** | — | MPL-2.0 / MIT OR Apache / Apache-2.0 | ✅ decode; cpal uses OS audio APIs | 2026-08 to 2026-10 | **USE.** Symphonia demuxes and decodes MP3, AAC, FLAC, Vorbis, WAV, ALAC, and MKV/MP4/OGG containers. MPL-2.0 is fine unmodified. |
| Video: thumbnails and frames | `ffmpeg-sidecar` | **2.6.0** | MIT | ❌ (spawns an external `ffmpeg` binary) | 2026-09-30 · 2.0 M | **USE (optional feature).** Process isolation is a *plus* for hostile media, and nothing is linked, so there is no LGPL/GPL issue. Requires shipping or locating ffmpeg. `ffmpeg-next` 9.0.0 (WTFPL bindings, links libav) → **AVOID**. |
| Video: pure-Rust decoding | `rust_h264` 0.4.0, `rusty_h264` 0.16.0, `rust_h265` 0.1.0, `rusty_h265` 0.6.0, `rusty_vp9` 0.1.2, `rav1d` 1.1.0 (AV1, BSD-2, port of dav1d with heavy `unsafe`) | — | mixed permissive | ✅ | 2026, mostly new | **EVAL / experimental.** Pure-Rust H.264/H.265 appeared in 2026 and is unproven. Demux with `re_mp4` 0.5.1 (MIT) or `matroska-demuxer` 0.8.1. `re_video` 0.38.1 (rerun) works but drags in the Rerun stack and uses ffmpeg-CLI for H.264. **Recommendation:** metadata via `nom-exif`; thumbnails via ffmpeg-sidecar when available; "open in external player" for playback; pure-Rust decode only as a later stretch goal. `egui-video` 0.9.0 (egui 0.29, ffmpeg) → AVOID. |
| PDF rendering | `hayro` (+ `hayro-interpret`, `-syntax`, `-svg`) | **0.8.0** | Apache-2.0 OR MIT | ✅ (own JPEG2000/JBIG2/CCITT decoders) | 2026-10-04 · 3.4 M | **USE.** The most complete pure-Rust renderer (passes 1 400+ PDFs from the pdf.js and PDFBox suites). It labels itself "experimental", and performance is not yet tuned: render pages off-thread and cache the textures. Gaps: knockout groups and non-embedded CID fonts. Alternatives: `pdfium-render` 0.9.4 (❌ needs the pdfium shared library), `mupdf` 0.8.0 (❌ AGPL → AVOID), `pdf-render` 1.0.0 (new, 2 k downloads), and `pdf` 0.10.0 (parser only). |
| HTML shown as text | `html2text` **0.17.3** (+ `ammonia` 4.2.3 to sanitise) | — | MIT / MIT OR Apache | ✅ | 2026-10-12 · 6.6 M | **USE.** Its "rich" mode yields annotated spans that map onto an egui `LayoutJob` (bold, links, headings). **Never** execute HTML or fetch remote resources. `htmd` 0.5.5 converts HTML→Markdown if we prefer to render with `egui_commonmark`. |

### 1.1 egui multi-viewport (pop-out windows): how it works in 0.36.2

Signatures, verbatim from `egui-0.36.2/src/context.rs`:

```rust
pub fn show_viewport_deferred(&self, new_viewport_id: ViewportId, viewport_builder: ViewportBuilder,
    viewport_ui_cb: impl Fn(&mut Ui, ViewportClass) + Send + Sync + 'static);
pub fn show_viewport_immediate<T>(&self, new_viewport_id: ViewportId, builder: ViewportBuilder,
    viewport_ui_cb: impl FnMut(&mut Ui, ViewportClass) -> T) -> T;
```

- **Both** must be called **every frame** while the window should exist, with a **unique `ViewportId`**.
  To close a window, stop calling the function. Detect the user closing it via
  `ctx.input(|i| i.viewport().close_requested())` inside the callback, then clear your "is popped out" flag.
- **Immediate**
  - Runs the `FnMut` right away, on the main thread, and can borrow local state (no `Send`/`Sync`).
  - Parent and child **repaint together**: N windows cost roughly N times the CPU.
    [#4963](https://github.com/emilk/egui/issues/4963) reports a large FPS drop.
  - Fine for short-lived dialogs.
- **Deferred**
  - The `Fn + Send + Sync + 'static` callback is stored and re-invoked whenever *that* viewport needs a
    repaint, independently of the parent.
  - State must live in `Arc<RwLock<…>>`/`Arc<Mutex<…>>` or behind channels.
  - Calling again with the same id replaces the callback.
  - **Use deferred for every pop-out viewer** (hex, text, media, map, graph, timeline). Give each viewer an
    `Arc<ViewerState>` and an `egui::Context`-driven repaint (`ctx.request_repaint_of(id)`).
- **Embedding fallback**
  - If `ctx.embed_viewports()` is true (backends without multi-window support, eframe on the web, or a
    kittest harness), the callback runs immediately inside an `egui::Window`, with
    `ViewportClass::EmbeddedWindow`.
  - Code must handle that class and never assume a separate OS window.
- **Threading**
  - All viewports are still painted on the **main thread**. On macOS, winit/AppKit require it.
  - "Deferred" means *scheduled separately*, not *parallel*. Heavy work stays on worker pools, and frames
    only read pre-computed state.
- **Open upstream issues to design around (as of 2026-10-14):**
  - macOS:
    - [#8259](https://github.com/emilk/egui/issues/8259): a child viewport flickers or aborts when opened from a fullscreen or maximised root window.
    - [#8085](https://github.com/emilk/egui/issues/8085): a regression where switching macOS Spaces destroys *immediate* viewports and recentres them. Another reason to use deferred.
    - [#8246](https://github.com/emilk/egui/issues/8246): `ViewportCommand::Minimized(true)` makes the app unresponsive.
    - [#7115](https://github.com/emilk/egui/issues/7115): Cmd+Q bypasses `confirm_exit`. Persist window and case state continuously.
    - [#8671](https://github.com/emilk/egui/issues/8671): an occluded or minimised window doesn't answer AccessKit requests.
  - Cross-platform:
    - [#8466](https://github.com/emilk/egui/issues/8466): a deferred viewport doesn't redraw while the root has focus (Windows).
    - [#7686](https://github.com/emilk/egui/issues/7686): the main viewport is blocked while deferred viewports render.
    - [#4842](https://github.com/emilk/egui/issues/4842): crash when closing a deferred viewport.
    - [#8470](https://github.com/emilk/egui/issues/8470): showing viewports from `App::logic` breaks. Only call `show_viewport_*` from `ui`/`update`.
    - [#8683](https://github.com/emilk/egui/issues/8683): cross-viewport hover bleed.
    - [#5145](https://github.com/emilk/egui/issues/5145): Wayland freeze when a viewport is behind the main window.
    - [#3556](https://github.com/emilk/egui/issues/3556): tracking issue for multi-viewport improvements.
  - **Mitigations:**
    - Cap concurrently open pop-outs (for example 8).
    - Persist the pop-out set and geometry with `ViewportBuilder::with_position/with_inner_size`. 0.35 added `with_monitor`.
    - Make every viewer able to re-dock.
    - Add an "embed all viewports" safe-mode setting (`ctx.set_embed_viewports(true)`).

---

## 2. Disk image containers

| Format | Candidates (version · licence · pure · maint.) | Verdict |
|---|---|---|
| Raw / split raw (`.001`, `.aa`, `.dd`) | — | **IN-HOUSE** (trivial `ReadAt` over a sorted segment list; already in the `pm-image` crate). |
| E01/Ex01/L01 (EWF) | `ewf` 0.4.10 (Apache-2.0, ✅, h4x0r, created 2026-03, 16 k dl). `ewf-image` 0.6.0 (Apache-2.0, ✅, **read+write**, 432 dl). `keramics-formats` ewf (0.0.1). `exhume_body` 0.5.7 (**GPL-3.0**). `libewf` ❌ excluded by ADR-001. | **IN-HOUSE** reader (the libyal EWF spec is thorough: chunk tables, zlib, Adler-32, MD5/SHA1 hash sections, v2 Ex01 with bzip2). Use `ewf` and `keramics` as **ORACLE**s in tests. Use `ewf-image` (or `ewfacquire` in Docker) to *write* E01 fixtures. |
| AFF4 | `aff4` 0.2.6 (Apache, h4x0r, 2026). `aff4-image` 0.2.0 (experimental, 46 dl). | **IN-HOUSE later.** AFF4 is a zip container + RDF/Turtle + snappy/lz4/deflate chunks. Building blocks: `zip`, `snap`, `lz4_flex`. Low priority. |
| VMDK | `vmdk-core` 0.8.4 (Apache, h4x0r). `rust-img-vmdk` 0.5.1 (MIT, created 2026-10-06). keramics `vmdk`. | **IN-HOUSE** (monolithic/split sparse + flat descriptor + streamOptimized; about 600 lines). Fixtures via `qemu-img convert -O vmdk` (qemu 10.1.0 is installed). |
| VHD / VHDX | `vhd-core` 0.1.4, `vhdx-core` 0.4.1 (Apache, h4x0r). `vhdx` 0.1.0 (2023, stale). `rust-img-vhd(x)` 0.6.1 (new). keramics `vhd`/`vhdx`. | **IN-HOUSE** (fixed, dynamic, and differencing VHD; VHDX with BAT, log replay ignored but flagged). Fixtures: `qemu-img -O vpc` / `-O vhdx`. |
| QCOW2 | `qcow2-core` 0.3.4 (Apache, h4x0r). `qcow2-rs` 0.1.6 (async, 2024). `qcow2` 0.1.2 (2016). keramics `qcow`. | **IN-HOUSE** (L1/L2 tables, zlib/zstd compressed clusters, backing files). Fixtures: `qemu-img`. |
| DMG (UDIF) | `udif` 0.4.2 (MIT, Dil4rd, 2026-10, 3.9 k dl). `dmg-core` 0.1.5 (Apache, h4x0r). `apple-dmg` 0.5.0 (indygreg, 2024, writer-oriented). keramics `udif`, `sparseimage`, `sparsebundle`. | **IN-HOUSE** (koly trailer + plist `blkx` mish tables; chunk codecs: zlib (`flate2`), bzip2 (`bzip2`), ADC (in-house, tiny), LZFSE (`lzfse_rust` 0.2.1, MIT/Apache, ✅), LZMA (`lzma-rust2`)). Fixtures: `hdiutil create -format UDZO/UDBZ/ULFO/ULMO/UDSP/UDSB`. |
| VDI, Parallels, VSS | keramics only. `vshadow` 0.2.0 is **AGPL** → AVOID. | IN-HOUSE when needed (VSS is high value for Windows cases). |

## 3. Volume systems

| Need | Candidates | Verdict |
|---|---|---|
| MBR + extended (EBR chains) | `mbrman` **0.6.1** (MIT OR Apache, ✅, 2025-07, 541 k dl), `bootsector` 0.2.0 (2022), `mbr-partition-core` 0.6.3 (h4x0r) | **IN-HOUSE** parser (it must tolerate overlaps, loops, and gaps, and report unpartitioned space). **USE `mbrman`** to *write* fixtures. |
| GPT | `gpt` **4.1.0** (MIT, 2025-03, 2.1 M dl), `gptman` **3.1.1** (MIT OR Apache, 2026-04, 937 k dl), `hadris-part` 2.5.0 | **IN-HOUSE** parser (primary vs backup header, CRC mismatch, 512/4096 sector probing). **USE `gptman`** for fixtures. |
| Apple Partition Map | `apm-partition-core` 0.6.2 (h4x0r) | **IN-HOUSE** (tiny). |
| BSD disklabel / Sun / SGI | keramics `bsdlabel`, `sgilabel` | **IN-HOUSE** (tiny; low priority). |
| Linux LVM2 | `lvm2` 0.0.3 (MIT, 2022, stale), `lamlvm` 0.1.1 (no_std fork, 2026-06, 172 dl) | **IN-HOUSE** (text metadata area + linear/striped segments). Use `lvm2` as a reference. |
| BitLocker detection | — | **IN-HOUSE** (`-FVE-FS-` OEM id at offset 3, plus FVE metadata block signatures; also the Windows To Go / BitLocker To Go variants). |
| BitLocker decryption (later) | `bitlocker-core` 0.3.5 (Apache, h4x0r), keramics `bde`; crypto from RustCrypto (`aes`, `xts-mode`, `ccm`) | EVAL later. Recovery-key / password unlock is in Autopsy's scope only as "detect + flag". |
| LUKS1/2 | `luks` **0.4.5** (MIT, louib, 2026-09), `luks-core` 0.1.8 (Apache, h4x0r), keramics `luksde` | **IN-HOUSE** detection (`LUKS\xBA\xBE` + JSON header for v2). Decryption: EVAL `luks` later. |
| CoreStorage / FileVault 2 | `filevault-core` (h4x0r), keramics `cdsaencr` | Detect only (IN-HOUSE). |

## 4. Filesystems (read-only, forensic: deleted entries, slack, unallocated)

**No crate provides forensic-grade access.** That means orphan/deleted enumeration, allocation bitmaps
exposed as "unallocated runs", file slack, and tolerance for corruption. Every general crate below is
built for *live-tree* access. The plan is in-house drivers behind `pm-fs` traits, with crates used as
oracles and fixture writers.

| FS | Best existing crate(s) | Verdict |
|---|---|---|
| NTFS | `ntfs` **0.4.0** (Colin Finck, MIT OR Apache, ✅, last release **2023-06**, 436 k dl). It handles attributes, attribute lists, index B-trees, and data runs, and any MFT record number can be opened, so deleted records are reachable. `mft` **0.7.0** (omerbenamram, MIT/Apache, 2025-12): $MFT record parser + bodyfile. `ntfs-core` 0.9.8 (h4x0r). `ntfs-reader` 0.6.0 (Windows-oriented live MFT/USN). `rust-fs-ntfs` (new). | **IN-HOUSE** forensic NTFS. It needs: $Bitmap → unallocated, deleted/orphan records, ADS, resident slack, $I30 slack entries, LZNT1/WOF compression, EFS flagging, $LogFile, $UsnJrnl:$J, and $Secure. **ORACLE:** `ntfs` + `mft` + Sleuth Kit `fls/istat` in Docker. `ntfs` is stable but effectively unmaintained; forking it into the workspace is an acceptable shortcut. |
| FAT12/16/32 | `fatfs` **0.3.6** (MIT, 2023-01, 1.7 M dl; already a workspace test dependency). `hadris-fat` 2.5.0 (MIT, active). | **IN-HOUSE** (`pm-fs-fat` exists). Neither crate exposes 0xE5 deleted entries or LFN orphans. **USE `fatfs`** as a fixture writer. Also `newfs_msdos` / `hdiutil -fs MS-DOS`. |
| exFAT | `exfat-fs` 0.1.3 (MIT, 2025), `exfat` 0.1.0 (2023), `exhume_exfat` (**GPL-3**), `fstool` writes exFAT | **IN-HOUSE.** Fixtures: `newfs_exfat`, `hdiutil create -fs ExFAT`. |
| ext2/3/4 | `ext4-view` **1.0.0** (MIT OR Apache, ✅, no_std, 2026-09-14, 135 k dl; high quality, by Nicholas Bishop). `ext4` 0.9.0 (2021). `exhume_extfs` (**GPL-3**). | **IN-HOUSE** forensic layer (inode-table scan for deleted inodes, extent/indirect blocks, block bitmap, jbd2 journal). **ORACLE / EVAL:** `ext4-view` is good enough for the *allocated* tree and could back the live view. Fixtures: `fstool` (pure Rust), `arcbox-ext4` 0.1.2, or `mke2fs -d` in Docker. |
| HFS+/HFSX | `hfsplus` 0.4.0 (MIT, Dil4rd, 2026), `hfsplus-core` (h4x0r, 56 dl), keramics `hfs` | **IN-HOUSE** (catalog/extents B-trees, attributes file, decmpfs compression via zlib + `lzfse_rust` + LZVN). Fixtures: `hdiutil create -fs HFS+`, `newfs_hfs`. |
| APFS | `apfs` 0.4.1 (MIT, Dil4rd), `apfs-core` 0.2.6 (h4x0r), `hadris-apfs` 3.0.0-rc.1 ("experimental"), keramics `apfs` | **IN-HOUSE** (largest FS effort: container superblock, checkpoints, omap, fs-tree, snapshots, clones, decmpfs; encrypted volumes are flagged only). Fixtures: `hdiutil create -fs APFS`, `newfs_apfs`. |
| ISO9660 / Joliet / Rock Ridge / El Torito | `hadris-iso` **2.5.0** (MIT, ✅, 2026-10, read+write), `cdfs` 0.2.3 (2023), `iso9660` 0.1.1 ("incomplete") | **IN-HOUSE** reader (small; must handle multi-session and inconsistent PVD/SVD). **USE `hadris-iso`** for fixtures and as an oracle. Also `hdiutil makehybrid -iso -joliet`. |
| UDF | `hadris-udf` **2.5.0** (MIT, read+write), `udf-core` (h4x0r) | EVAL `hadris-udf` as a reader; otherwise IN-HOUSE. Fixtures: `newfs_udf`, `hdiutil makehybrid -udf`. |
| XFS | `xfs-core` 0.1.6 (h4x0r), `rust-fs-xfs` 0.14.1 (new), keramics `xfs` | **IN-HOUSE** (lower priority). Fixtures: `mkfs.xfs` in Docker; `fstool` writes XFS. |
| Btrfs | `btrfs-disk` **0.13.0** (rustutils, MIT OR Apache, 2026-05; a full userspace on-disk library), `btrfs-core` (h4x0r) | EVAL `btrfs-disk` (read path); otherwise IN-HOUSE. Low priority. |
| YAFFS2 | none on crates.io | **IN-HOUSE** (spare-area/OOB layout probing, as TSK does). Low priority. |
| Multi-FS fixture builder | `fstool` **0.4.35** (MIT, ✅, 2026-09). Writes ext2/3/4, FAT12/16/32, exFAT, NTFS, XFS, HFS+, HFS, squashfs, and MBR/GPT layouts from a TOML spec. API is "unstable until v0.5". | **USE as a dev-dependency / CLI** to generate fixtures. Pin the exact version. |

## 5. Windows, macOS, and app artefacts

| Artefact | Candidates | Verdict |
|---|---|---|
| Registry hives (regf) | `notatin` **1.0.1** (Apache-2.0, ✅, Stroz Friedberg; single release 2023-08, 60 k dl). Recovers **deleted/modified keys and values**, applies **transaction logs** (.LOG1/.LOG2), and has a cell-level API. `nt-hive` 0.3.0 (**GPL-2.0+**, no recovery). `frnsc-hive` 0.13.4 (MIT, tied to forensic-rs traits). `regf-rs` 0.1.1 (new). | **USE `notatin`** (vendor/fork if fixes are needed; upstream is quiet). Use `regf-rs`/`sootmark-registry` only as oracles. AVOID `nt-hive` (licence). |
| EVTX | `evtx` **0.12.3** (omerbenamram, MIT/Apache, ✅, 2026-09-22, 315 k dl). Multi-threaded, with JSON/XML output. | **USE.** Chunk carving from unallocated is IN-HOUSE, reusing the evtx chunk parser on carved 64 KiB chunks. |
| LNK | `lnk` **0.6.4** (MIT, ✅, 2026-03; parse **and write**, so it doubles as a fixture generator). `lnk_parser` 0.4.3 (MIT, forensic-oriented, 2026-02). | **USE `lnk`.** Shell-item (PIDL) decoding is IN-HOUSE; `shellitem` 0.2.3 (h4x0r) is a reference. |
| Prefetch (incl. Win10+ MAM) | `frnsc-prefetch` 0.13.3 (MIT, 2025), `prefetch-core` 0.1.1 (h4x0r). MAM uses LZXpress-Huffman: `xpress-huffman` 0.1.3 (Apache, ✅, h4x0r, 2026-08). `rust-lzxpress` 0.7.1 (MIT, 2021) handles **plain LZ77 + LZNT1** only (no Huffman). | **IN-HOUSE** SCCA parser (versions 17/23/26/30/31) and an **IN-HOUSE** MS-XCA LZ77+Huffman decoder (about 300 lines, spec-defined). `xpress-huffman` is the ORACLE. LZNT1 (needed for NTFS too) is also IN-HOUSE. |
| ESE / JET Blue (SRUM, WebCache, Windows Search) | `libesedb` 0.2.7 (**LGPL-3.0+**, ❌ C), `ese-core` 0.2.2 (Apache, h4x0r), `ese-parser` 0.1.0 (MIT, new, 456 dl), `sootmark-ese` 0.1.1, `srum-parser` 0.4.0 | **IN-HOUSE** pure-Rust ESE reader (pages, B+trees, long values, tagged columns, 7-bit/xpress compressed columns, dirty-shutdown tolerance). Use `libesedb` only as a Docker/CI **ORACLE**, never linked into the product. |
| OLE CFB (jump lists, legacy Office, thumbs.db) | `cfb` **0.15.0** (MIT, ✅, mdsteele, 2026-09, 72 M dl, read/write) | **USE.** It does not expose orphaned or unallocated directory entries or sectors; that recovery is IN-HOUSE. The DestList stream is IN-HOUSE. |
| plist (XML/binary) | `plist` **1.10.1** (MIT, ✅, 2026-09, 80 M dl) | **USE.** NSKeyedArchiver object-graph resolution is IN-HOUSE (thin layer). |
| SQLite read (incl. WAL) | `rusqlite` **0.40.2** + `bundled` (MIT; ❌ compiles the SQLite C amalgamation, which is accepted as the case DB engine) | **USE** for parsing *intact* evidence DBs. Copy the DB + `-wal` + `-shm` to a temp dir and open the copy (`SQLITE_OPEN_READ_ONLY`). Opening evidence in place can checkpoint or modify it. `immutable=1` skips the WAL. |
| SQLite deleted-record recovery (freelist, unallocated cells, WAL frames, rollback journal) | `sqlite-decoder` 0.1.1 (2022, stale), `chat4n6-sqlite-forensics` 0.1.1 (h4x0r), `sootmark-sqlite` 0.2.0 | **IN-HOUSE** pure-Rust page parser (file format is fully documented). It is the highest-value artefact component after the filesystems. |
| LevelDB (Chrome Local Storage, IndexedDB, Session) | `rusty-leveldb` **4.0.1** (MIT, ✅, 2025-10). A full DB, but it **writes** (LOCK, LOG, compaction), so use it only for building fixtures. `snap` **1.1.2** (BSD-3, ✅) handles Snappy. | **IN-HOUSE** raw reader for `.ldb`/`.log`/MANIFEST that keeps deleted and old sequence-number records. IndexedDB key decoding and the Blink/V8 `SerializedScriptValue` deserialiser are IN-HOUSE (`chromium-storage-indexeddb-core` 0.2.0 is a reference). |
| Safari `.binarycookies` | `safari-binarycookies` 1.0.0 (Apache-2.0, 2026-06), `binary-cookies` 0.3.1 (**LGPL**), `binarycookies` 0.1.3 | **IN-HOUSE** (about 150 lines). The Apache crate is acceptable too. |
| $MFT / $UsnJrnl / $LogFile | `mft` 0.7.0 (see above), `usnrs` 0.2.1 (Apache-2.0, Airbus CERT, 2025), `usnjrnl` 0.4.5 (no SPDX licence → AVOID) | $MFT: part of the in-house NTFS layer (`mft` as ORACLE). USN v2/v3/v4: **IN-HOUSE**, with `usnrs` as ORACLE. **$LogFile: IN-HOUSE** (no crate). |
| Recycle Bin `$I`/INFO2 | `sootmark-recyclebin` 0.1.0 (19 dl) | **IN-HOUSE** (trivial). |
| thumbcache / iconcache / thumbs.db | `sootmark-imagecache` 0.1.0 (19 dl); thumbs.db = `cfb` | **IN-HOUSE.** |
| Protobuf | `prost` **0.14.4** (Apache-2.0) for known schemas; `protobuf` 3.7.2 (stable; 4.x is a different upb-based line) | **USE `prost`** where `.proto` files exist. Schemaless wire-format decoding (blackboxprotobuf-style) for unknown blobs is **IN-HOUSE** (about 200 lines). |
| Apple SEGB / Biome | `segb-core` 0.2.1 (h4x0r) | IN-HOUSE when needed. |

## 6. Email

| Need | Candidates | Verdict |
|---|---|---|
| MBOX | `mbox-reader` 0.2.0 (2019, stale), `mailbox-formats` 0.1.3 (MIT/Apache, 2026-06) | **IN-HOUSE** splitter (mboxo/mboxrd/mboxcl/mboxcl2, `From_` heuristics, byte offsets kept for provenance) → `mail-parser`. |
| EML / MIME | `mail-parser` **0.11.9** (Stalwart, Apache-2.0 OR MIT, ✅, 2026-09, 4.3 M dl). Lenient, with full RFC 5322/2045/2231 support, charset decoding, and nested message/rfc822. `mailparse` 0.18.0 (0BSD) as a fallback. | **USE `mail-parser`.** |
| PST / OST | `outlook-pst` **1.2.0** (Microsoft `microsoft/outlook-pst-rs`, MIT, ✅, 2026-03-17, 196 k dl). Clean-room MS-PST: NDB/LTP/messaging layers, **ANSI and Unicode PST**, attachments, named properties, crash recovery/AMap rebuild. `outlook-pst-rw` 1.2.13 (a fork adding writing, 2026-10). | **USE `outlook-pst`.** Gaps → IN-HOUSE extensions: **OST 2013+ (4 KiB pages, compressed blocks) does not appear to be supported** (source uses `PAGE_SIZE = 512`), and deleted/orphaned item recovery. Use `outlook-pst-rw` to build PST fixtures. |
| vCard / iCalendar | `calcard` **0.3.14** (Stalwart, Apache-2.0 OR MIT, 2026-09; lenient vCard 2.1/3/4 + iCal + JSContact) | **USE.** `vcard_parser` 0.2.3 (strict RFC 6350) is an alternative. `ical` 0.11.0 has no SPDX licence (Apache in the repo) and is stale. |

## 7. Documents and text extraction

| Need | Candidates | Verdict |
|---|---|---|
| PDF text | `pdf_oxide` **0.3.78** (MIT OR Apache, ✅, 2025-11 → 2026-09, 83 releases, 1.27 M dl; fast, with layout, markdown, and images). `pdf-extract` **0.12.1** (MIT, lopdf-based, mature, weaker on CJK/ToUnicode). `lopdf` 0.45.0 (low level). `hayro-interpret` (glyph-level, via a custom `Device`). | **USE `pdf_oxide`** with **`pdf-extract` as fallback** on error. Run both under a per-document timeout and memory cap; PDFs are hostile. |
| DOCX / XLSX / PPTX | `zip` **9.0.3** + `quick-xml` **0.42.0** (both MIT, ✅). `office_oxide` **0.1.14** (MIT OR Apache, 2026-04, 921 k dl; DOCX/XLSX/PPTX/DOC/XLS/PPT). `calamine` **0.36.1** (MIT; XLS/XLSX/XLSB/ODS). `docx-rs` 0.4.22 is generation-oriented. | **USE `calamine`** for spreadsheets. Use **`office_oxide`** for the rest. Keep a small IN-HOUSE OOXML text walker (`zip` + `quick-xml`) as fallback and for core/app/custom-properties metadata. |
| Legacy OLE Office (DOC/XLS/PPT) | `office_oxide` (as above), `calamine` (XLS), `cfb` + IN-HOUSE property-set (`\x05SummaryInformation`) parser | **USE `office_oxide`** (young: EVAL its robustness on fuzzed input). Metadata is IN-HOUSE. `office-crypto` 0.4.0 (MIT) detects and decrypts password-protected Office files. |
| RTF | `rtf-parser` **0.4.3** (MIT, 2026-06, 199 k dl) | **USE** (text only). A tolerant IN-HOUSE fallback that strips control words is recommended for broken RTF. |
| HTML → text | `html2text` **0.17.3** (MIT) | **USE.** `nanohtml2text` 0.3.0 is a zero-dependency fast path for indexing. |
| Encoding detection / decoding | `chardetng` **1.0.0** (Apache-2.0 OR MIT) + `encoding_rs` **0.8.42** (+ `encoding_rs_io` 0.1.8) | **USE** (the Firefox stack). UTF-16LE heuristics for Windows strings are IN-HOUSE. |
| Language detection | `whatlang` **0.18.0** (MIT, small, 69 languages, fast). `lingua` **1.8.0** (Apache-2.0, high accuracy, **large models**: per-language feature flags, hundreds of MB with all of them). | **USE `whatlang`** by default. Offer `lingua` with a few languages as an opt-in feature. |
| OCR | `ocrs` **0.13.1** + `rten` (ocrs pins `rten ^0.26`; the latest rten is 0.27.0) (MIT OR Apache, ✅, Robert Knight, 2026-09) | **USE (optional module).** Pure Rust with ONNX-derived models, downloaded or bundled separately (about 10 MB). Strong on **Latin script**, weak elsewhere. `oar-ocr` 0.10.0 uses ONNX Runtime (❌). `tesseract` 0.15.2 needs C++ (❌). |

## 8. Archives and compression

| Format | Crate · version · licence | Pure | Verdict / caveats |
|---|---|---|---|
| ZIP (incl. ZIP64, AES, deflate64, bzip2, lzma, xz, zstd, ppmd) | `zip` **9.0.3** (MIT, zip-rs/zip2, 2026-10-13, 300 M dl) | ⚠️ (`zstd` feature uses C; set features explicitly) | **USE** with `default-features = false, features = ["aes-crypto","deflate","deflate64","bzip2","lzma","xz","ppmd","time"]`, plus `zstd` via ruzstd if needed. Its parser assumes a sane central directory: local-header carving and recovery are IN-HOUSE. |
| 7z | `sevenz-rust2` **0.24.0** (Apache-2.0, hasenbanck, 2026-10, 1.7 M dl). The original `sevenz-rust` 0.6.1 stalled in 2024. | ✅ | **USE `sevenz-rust2`** (LZMA/LZMA2/BCJ/PPMd/AES). |
| RAR (v1.5 to RAR7) | `rars` **0.10.2** (Apache-2.0, ✅, bitplane, created 2026-05, 106 k dl). Read, write, and repair, including **RAR5**. Self-described as starting as an "agentic development experiment". `unrar` 0.5.8 needs C++ (❌, unRAR licence restrictions). | ✅ | **EVAL → USE `rars`** behind fuzzing and a cross-check against `7zz` in CI. No other pure-Rust RAR5 reader exists. |
| tar | `tar` **0.4.46** (MIT OR Apache) | ✅ | **USE.** |
| gzip / zlib / deflate | `flate2` **1.1.10** (default backend `miniz_oxide` 0.9.1 ✅). Optional `zlib-rs` 0.6.8 (✅, Zlib licence, faster). | ✅ | **USE `flate2` + `zlib-rs`.** Multi-member gzip needs `MultiGzDecoder`. |
| bzip2 | `bzip2` **0.6.1**: defaults to `libbz2-rs-sys` 0.2.5, a pure-Rust port | ✅ | **USE.** |
| xz / lzma / lzma2 | `lzma-rust2` **0.22.0** (Apache-2.0, ✅). `liblzma` 0.4.8 is C (❌). `xz2` 0.1.7 is stale and C. `lzma-rs` 0.3.0 is stale (2023). | ✅ | **USE `lzma-rust2`.** |
| zstd | `ruzstd` **0.9.1** (MIT, ✅, decoder + basic encoder). `zstd` 0.14.2 uses C (❌). | ✅ | **USE `ruzstd`** for evidence. `tantivy` brings the C `zstd` in its default features; disable that feature (§13). |
| LZ4 | `lz4_flex` **0.14.0** (MIT, ✅) | ✅ | **USE.** |
| Snappy | `snap` **1.1.2** (BSD-3) | ✅ | **USE.** |
| CAB (MSZIP, LZX) | `cab` **0.6.0** (MIT, ✅, 2024; uses `lzxd` 0.2.7) | ✅ | **USE.** Quantum compression is unsupported, which is rare. |
| LZX (CHM, WIM) | `lzxd` **0.2.7** (MIT OR Apache) | ✅ | **USE.** CHM and WIM container parsing is IN-HOUSE. |
| LZFSE / LZVN (Apple) | `lzfse_rust` **0.2.1** (MIT OR Apache, ✅). LZVN: `lzvn-core` 0.1.3 (h4x0r) or IN-HOUSE (tiny). | ✅ | **USE `lzfse_rust`.** LZVN is IN-HOUSE. |
| LHA/LZH, cpio, ar | `delharc` 0.8.0, `cpio` 0.4.1 | ✅ | Optional. |
| ISO as an archive | see §4 (`hadris-iso` / in-house) | ✅ | — |
| "Everything" via libarchive | `compress-tools` 0.16.1 (❌ libarchive) | ❌ | **AVOID** (ADR-001). |

## 9. Media metadata and decoding

| Need | Crate · version · licence | Pure | Verdict / caveats |
|---|---|---|---|
| EXIF (JPEG/TIFF/HEIF/PNG/WebP) | `nom-exif` **3.8.0** (MIT per its LICENSE file; crates.io shows "non-standard", 2026-09). Covers images **and video** (MOV/MP4/3GP/MKV GPS, creation time, track info). `kamadak-exif` **0.6.1** (BSD-2, 2024-11, 15 M dl, mature, image-only). `little_exif` 0.6.23 (read/write). | ✅ | **USE `nom-exif`** as primary (one crate for images and video) and **`kamadak-exif`** as fallback. Keep raw IFD bytes for provenance. |
| Image decode | `image` **0.25.10** (JPEG via `zune-jpeg` 0.5.15, PNG, GIF, WebP via `image-webp`, TIFF, BMP, ICO, QOI, EXR, TGA…); `jxl-oxide` 0.12.6 for JPEG-XL | ✅ (AVIF *decode* needs `avif-native` → dav1d ❌) | **USE** `default-features = false` with an explicit format list. Set `image::Limits` (max allocation and dimensions). |
| HEIC/HEIF decode | `zune-heic` **0.5.0** (MIT OR Apache OR Zlib, own HEVC decoder, by the zune author; **published 2026-10-11**, 52 dl). `heic-rs` 0.1.4 (MIT/Apache, 2026-09, 37 k dl). `heif-oxide` 0.1.0 (via `rust_h265`). `heic` 0.1.6 (imazen, **AGPL** or commercial → AVOID). `libheif-rs` 3.0.0 (❌ C, LGPL libheif). | ✅ | **EVAL `zune-heic`** (trusted author, brand new) with `heic-rs` as second choice, behind fuzzing. Metadata via `nom-exif` works regardless. |
| Thumbnails / resize | `fast_image_resize` **6.1.0** (MIT OR Apache, SIMD) | ⚠️ (`unsafe` SIMD internally) | **USE.** |
| Perceptual hash (similar images) | `image-hasher` **3.1.1** (MIT OR Apache) | ✅ | Optional. |
| Audio tags | `lofty` **0.25.4** (MIT OR Apache) | ✅ | **USE.** |
| Video container metadata | `nom-exif` (above), `re_mp4` 0.5.1 (MIT), `matroska-demuxer` 0.8.1 | ✅ | `mp4` 0.14.0 (2023) and `mp4parse` 0.17.0 (2023) are stale. |
| Video thumbnails | `ffmpeg-sidecar` 2.6.0 (external binary) | ❌ | Optional feature (see §1). |
| MIME / file-type detection | `file-format` **0.29.0** (MIT/Apache, ✅, about 600 formats with optional deep "readers" for CFB, ZIP, MP4, PDF, SQLite, EBML…). `infer` **0.22.0** (MIT, small). `pure-magic` **0.4.1** + `magic-db` 0.6.0 (**GPL-3.0 OR BSD-2**: choose BSD-2; libmagic re-implementation with an embedded compiled `magic` DB). `libmagic-rs` 0.12.7 (Apache-2.0, 2026, young). `tree_magic_mini` 3.2.2 (needs the system shared-mime-info DB, or GPL data). | ✅ | **USE `file-format`** (`reader` features) as the primary signature engine plus IN-HOUSE forensic signatures (registry hives, EVTX, PF/MAM, ESE, LNK, thumbcache, LevelDB…). **EVAL `pure-magic`** under BSD-2 for libmagic-parity strings. `mime_guess` 2.0.5 is extension-only and should be used only for "extension mismatch" checks. |

## 10. Hashing

| Need | Crate · version · licence | Pure | Notes |
|---|---|---|---|
| MD5 / SHA-1 / SHA-256/512 | `md-5` **0.11.0**, `sha1` **0.11.0**, `sha2` **0.11.1**, `digest` **0.11.3** (RustCrypto, MIT OR Apache) | ✅ | **USE.** The 0.11 line has no `asm` feature. SHA-1 and SHA-2 use runtime CPU detection for SHA-NI / ARMv8 crypto intrinsics. MD5 is inherently serial, so hash multiple files in parallel rather than one file faster. Compute MD5+SHA-1+SHA-256 in **one pass** over each 1 MB read buffer. |
| BLAKE3 | `blake3` **1.8.7** (CC0 / Apache / Apache-LLVM) | ⚠️ (SIMD via C/asm by default; the `pure` feature forces Rust intrinsics) | **USE.** Has `rayon` + `mmap` features for multi-GB files. Uses SSE4.1/AVX2/AVX-512/NEON. |
| Non-crypto fast hash (dedupe keys) | `xxhash-rust` 0.8.19 (BSL-1.0), `crc32fast` 1.5.2 | ✅ | USE. |
| ssdeep (CTPH) | `fuzzyhash` **0.2.2** (MIT, pure-Rust port, 2023, stale but small). `ffuzzy` 0.3.16 (best quality, **GPL-2.0+**). `ssdeep` 0.7.0 (GPL-3, C). | ✅ | **IN-HOUSE** CTPH (about 400 lines; the algorithm is public). Use `fuzzyhash` as a dev ORACLE. AVOID GPL crates in the product. |
| TLSH | `tlsh2` **1.1.0** (Apache-2.0 OR BSD-3, vthib, 2025-10), `fast-tlsh` 0.1.10 (Apache OR MIT) | ✅ | **USE `tlsh2`.** |

## 11. Hash sets / NSRL

No crate needed. NSRL RDS v3 is distributed as SQLite (`rusqlite`). Project VIC/CAID uses JSON (`serde_json`). Our
own hash DBs are SQLite or Postgres. Use a sorted, memory-mapped binary index (IN-HOUSE) for fast lookups.

## 12. YARA

| Crate | Version | Licence | Pure | Status / caveats |
|---|---|---|---|---|
| `yara-x` (VirusTotal) | **1.21.0** (2026-09-29; 1.x stable since 2025, 43 releases) | BSD-3-Clause | ✅ (compiles rules to WASM and runs them on **wasmtime ^45.0.3**) | **USE.** VirusTotal's production engine, with very high YARA compatibility and better diagnostics. Modules: pe, elf, macho, dotnet, dex, lnk, zip, crx, hash, math, string, time, vt; some are feature-gated. Caveats: heavy build (wasmtime, protobuf codegen via `generate-proto-code`); pins its own wasmtime (see §0.4); MSRV 1.93. Scan buffers via the `Scanner` API; **compile rules once and share**. |
| `boreal` | **1.3.0** (2026-09) | MIT OR Apache-2.0 | ✅ (no wasmtime) | Lighter, YARA-compatible alternative. Good fallback if binary size or compile time is a problem. |
| `yara` (libyara bindings) | 0.32.0 | MIT OR Apache | ❌ | AVOID (ADR-001). |

## 13. Search and pattern matching

| Need | Crate · version · licence | Pure | Notes |
|---|---|---|---|
| Full-text index | `tantivy` **0.26.2** (MIT, 2026-09-08; 0.27.0 is in the changelog but not published) | ⚠️ | **USE** `default-features = false, features = ["mmap","stopwords","lz4-compression","stemmer"]`. The default `columnar-zstd-compression` pulls in the C `zstd`. MSRV 1.86. Per-case index directory. One `IndexWriter` per index, with commits batched every N docs or T seconds. Store only object id + page/chunk id; text lives in our DB or on disk. |
| Tokenisers | `tantivy` built-ins + `lindera` **6.2.0** (MIT; Japanese/Korean/Chinese dictionaries, large) / `tantivy-jieba` 0.21.0 (Chinese), `unicode-segmentation` 1.13.3, `icu_segmenter` 2.3.0 (Unicode-3.0) | ✅ | Provide an "ngram fallback" field for CJK, as Autopsy's Solr does, then optional morphological analysers. |
| Regex over text/bytes (keyword search on unallocated) | `regex` **1.13.1** (`regex::bytes`), `regex-automata` **0.4.18** (multi-pattern DFA, streaming via `hybrid`) | ✅ | **USE.** Build a single multi-pattern `meta::Regex` per keyword list. For raw-disk scans, use chunked reads with an overlap of the maximum match length. |
| Literal multi-pattern | `aho-corasick` **1.1.5** (Unlicense/MIT), `daachorse` **5.0.0** (MIT OR Apache; very large dictionaries), `memchr` **2.8.3** | ✅ | **USE** `aho-corasick`, and `daachorse` for very large hash or keyword lists. |
| Hyperscan-class engine | `vectorscan-rs` 0.0.6 (builds C++ vectorscan ❌), `hyperscan` 0.3.2 (2022 ❌) | ❌ | **AVOID.** `regex-automata` + `aho-corasick` are sufficient; no pure-Rust Hyperscan equivalent exists. |

## 14. Database layer (SQLite single-user, PostgreSQL multi-user)

| Option | Version | Verdict |
|---|---|---|
| `rusqlite` (+ `bundled`) | **0.40.2** (MIT, 2026-08, 125 M dl; `libsqlite3-sys` 0.38.2) | **USE for SQLite.** Synchronous, minimal overhead, full SQLite feature access (WAL, `prepare_cached`, `Transaction`, `backup`, `blob`, `hooks`, `functions` for regexp/collation). Fits ingest worker threads directly. |
| `postgres` / `tokio-postgres` (+ `postgres-types`) | **0.19.14** / **0.7.18** (MIT OR Apache, 2026-06) | **USE for Postgres.** `postgres` (sync) for the storage trait used by ingest threads; `tokio-postgres` for the LISTEN/NOTIFY listener task. Binary `COPY … FROM STDIN (FORMAT binary)` for bulk inserts. |
| Pools | `r2d2` 0.8.10 (stable since 2022) + `r2d2_sqlite` **0.35.0** (needs rusqlite ^0.40 ✔) + `r2d2_postgres` 0.18.2 (2024); or `deadpool` 0.13.1 / `deadpool-postgres` **0.14.2** / `deadpool-sqlite` 0.14.0 (async) | **USE `deadpool-postgres`** (async; also usable from sync code via a runtime handle) **or `r2d2_postgres`** for purely sync access. For SQLite: one writer connection (a dedicated thread owning the write `Connection`, fed by a channel) plus an `r2d2_sqlite` reader pool. SQLite WAL allows concurrent readers and a single writer. |
| Query building across dialects | `sea-query` **1.0.2** (MIT OR Apache) | **USE** (`SqliteQueryBuilder` / `PostgresQueryBuilder`). **Do not** use `sea-query-rusqlite` 0.8.0 (needs rusqlite ^0.38, a links conflict). Write a 50-line value binder for rusqlite. `sea-query-postgres` 0.6.1 is fine. |
| Migrations | `refinery` **0.10.0** (MIT; supports rusqlite ≤ 0.40 **and** postgres/tokio-postgres ≤ 0.19/0.7) | **USE**: one migration tool for both backends, with SQL files per dialect. `rusqlite_migration` 2.6.0 is SQLite-only. |
| `sqlx` | **0.9.0** (2026-05-21; MSRV 1.94) | **AVOID.** `sqlx-sqlite` needs `libsqlite3-sys < 0.38`, which **cannot coexist with rusqlite 0.40** (links conflict). It is async-only, which adds per-row overhead for SQLite bulk ingest. Compile-time checked queries need a live DB or offline data per dialect. |
| `diesel` (+ `diesel-async` 0.9.2, `diesel_migrations` 2.3.2) | **2.3.14** (2026-10; `libsqlite3-sys < 0.39` ✔ compatible) | Viable alternative (sync, both backends, `MultiConnection`). Heavier compile times and generic-type complexity. Not recommended for a schema tuned per backend (ADR-002). |
| `sea-orm` | **2.0.4** (built on sqlx 0.9) | **AVOID** (inherits the sqlx conflict; ORM overhead on hot paths). |

**Recommendation:** a `pm-store` trait with two hand-written backends, **rusqlite** and **postgres**. Shared
SQL comes from `sea-query`; migrations use `refinery`; Postgres bulk loads use `COPY`.

## 15. Concurrency and infrastructure

| Need | Crate | Version | Licence | Notes |
|---|---|---|---|---|
| Data parallelism | `rayon` | **1.12.0** | MIT OR Apache | Ingest pipelines and hashing. |
| Channels | `crossbeam-channel` | **0.5.17** | MIT OR Apache | Already in the workspace. `crossbeam` 0.8.5 is the umbrella crate. `flume` 0.12.0 is an alternative with async support. |
| Async runtime | `tokio` | **1.53.3** | MIT | For multi-user networking only (NATS, gRPC, PG LISTEN). Keep it off the GUI thread. |
| Logging | `tracing` **0.1.44** + `tracing-subscriber` **0.3.23** | — | MIT | Add `tracing-appender` for per-case log files. |
| Errors | `thiserror` **2.0.21** / `anyhow` **1.0.104** | — | MIT OR Apache | `thiserror` in libraries, `anyhow` in binaries and tests. |
| Serialisation | `serde` **1.0.229**, `serde_json` **1.0.154** | — | MIT OR Apache | — |
| Platform dirs | `directories` | **6.0.0** | MIT OR Apache | 2025-01, stable. |
| File watching | `notify` | **8.2.0** | CC0-1.0 | 9.0.0-rc.5 exists; stay on 8.x. Used for the auto-ingest / hot-folder feature. |
| mmap | `memmap2` | **0.9.11** | MIT OR Apache | `unsafe` is confined to that crate. **Never** mmap evidence on network shares or removable media; truncation causes SIGBUS. Prefer positional reads (`ReadAt`). |
| Locks | `parking_lot` | **0.12.5** | MIT OR Apache | — |
| Concurrent map | `dashmap` | **6.2.1** | MIT | 7.0.0-rc2 exists. |
| Caches | `moka` **0.12.16** / `lru` **0.18.5** | — | MIT OR Apache / MIT | Block cache for image layers, thumbnail cache. |
| Time | `jiff` | **0.2.38** | Unlicense OR MIT | Already chosen. `chrono` 0.4.45 is only needed where third-party crates require it. |
| IDs | `uuid` | **1.28.0** | Apache OR MIT | — |
| Binary parsing helpers | `zerocopy` **0.8.63**, `binrw` **0.15.2**, `winnow` **1.0.4**, `nom` **8.0.0**, `bstr` **1.13.1**, `widestring` **1.2.1** | — | permissive | Preference: hand-written checked accessors (`pm_core::Bytes`, ADR-004). `zerocopy` with `FromBytes` for fixed headers. |
| CLI | `clap` | **4.6.7** | MIT OR Apache | Headless ingest / report CLI. |
| Misc. | `tempfile` 3.27.0, `walkdir` 2.5.0, `camino` 1.2.6, `bytesize` 2.7.0 (SI by default, per ADR-005), `indicatif` 0.18.6 (CLI) | — | permissive | — |

## 16. Multi-user (Autopsy uses PostgreSQL, Solr, and ActiveMQ)

| Need | Crate | Version | Licence | Design notes |
|---|---|---|---|---|
| Change notifications | `tokio-postgres` (`Connection::poll_message` → `AsyncMessage::Notification`) | **0.7.18** | MIT OR Apache | **LISTEN/NOTIFY** on a **dedicated, non-pooled** connection. Payload limit is under 8 000 bytes: send ids only, and clients re-query. Notifications are lost while disconnected, so resync on reconnect via a monotonic `change_seq` table. |
| Locks | plain SQL (`pg_advisory_lock(bigint)`, `pg_try_advisory_xact_lock`) | — | — | Session-level advisory locks **must** live on a dedicated connection; returning one to a pool leaks the lock. Prefer `*_xact_lock` inside transactions. Hash `(case_id, resource)` → i64. |
| Event bus (replaces ActiveMQ) | `async-nats` | **0.50.0** | Apache-2.0 | Core NATS pub/sub for ingest progress and UI events. **JetStream** for durable job queues (auto-ingest), KV for node heartbeats, Object Store for small artefacts. |
| Index server RPC (replaces Solr) | `tonic` **0.14.6** + `tonic-prost` **0.14.6** + `prost` **0.14.4**; build: `tonic-prost-build` **0.14.6** | — | MIT / Apache-2.0 | Since tonic 0.14, prost integration lives in the separate `tonic-prost` / `tonic-prost-build` crates. Server-streaming search results. Use TLS via rustls (tonic `tls-ring`/`tls-aws-lc` features). The server wraps tantivy (one writer per case index). |

## 17. Plugins

| Option | Crate · version | Licence | Stability notes |
|---|---|---|---|
| Python (embedded CPython) | `pyo3` **0.29.3** (2026-09-30; **0.30.0 is in the changelog** dated 2026-10-13, dropping Python 3.8 and adding 3.15, but is **not yet on crates.io**) | MIT OR Apache | Use `auto-initialize` or explicit `Python::initialize`. All Python work goes through `Python::attach` (formerly `with_gil`). Release with `Python::detach` around Rust work. **Caveats:** you must **ship a matching CPython** (or use python-build-standalone) for macOS/Windows; native wheels of plugins must match it. The GIL serialises Python plugins unless you use free-threaded 3.14t/3.15t, which still has ecosystem gaps. An interpreter crash kills the app, so consider running Python plugins in a **separate worker process** over the same plugin RPC as WASM. pyo3 breaks its API on every 0.x minor release. |
| WASM (component model) | `wasmtime` **49.0.2** / `wasmtime-wasi` **49.0.2** (50.0.0-rc.1 pending); guest side `wit-bindgen` **0.62.0** | Apache-2.0 WITH LLVM-exception | The component model and WASI 0.2 (P2) are stable in wasmtime. WASI 0.3 (async) is still maturing. wasmtime ships a **new major every month** and supports only recent majors with security fixes, so plan monthly bumps. A stable **WIT** interface (`postmortem:plugin@x.y.z`) is our real ABI. Use fuel/epoch interruption + `ResourceLimiter` for CPU and memory caps. Expose evidence via a `read-at(offset, len)` host import, never a filesystem pre-open. **Coordinate the version with `yara-x`**: pinning 45.x avoids two wasmtimes. |
| WASM (simpler) | `extism` **1.41.0** | BSD-3 | Core modules rather than components, with a simpler host SDK. Fine for a quick start, but WIT gives better typed contracts. Not recommended long-term. |

## 18. Reports and exports

| Need | Crate | Version | Licence | Notes |
|---|---|---|---|---|
| XLSX | `rust_xlsxwriter` | **0.99.1** | MIT OR Apache | **USE.** Constant-memory mode for large tables, hyperlinks, images, autofilter. |
| HTML templating | `askama` | **0.16.1** | MIT OR Apache | **USE** for built-in report templates (compile-time checked, autoescaping). |
| User-editable templates | `minijinja` | **3.0.0** | Apache-2.0 | **USE** for custom report templates (Jinja2 syntax, sandboxed, autoescape). `tera` 2.4.0 is an alternative. `maud` 0.27.0 (last release 2025-02) also works but is less flexible. |
| PDF report | `typst` **0.15.1** (Apache-2.0) or `krilla` **0.8.2** (MIT OR Apache; PDF writer by the hayro author) / `printpdf` 0.12.8 (MIT) | ✅ | Optional. Generating HTML and printing it is the simplest route; typst gives high-quality output. |
| KML / KMZ | `kml` **0.14.0** (georust, MIT OR Apache) + `zip` for KMZ; `geojson` 1.0.0, `geo-types` 0.7.20 | ✅ | **USE.** |
| STIX 2.1 | `rstix` 0.24.0 (MIT, inside timescale/rsigma, 304 dl), `stix2` 0.1.1 (BSD-3, 165 dl), `stix-pattern` 0.2.0 | ✅ | **IN-HOUSE** serde structs for the SDO/SCO subset we emit (indicator, observed-data, file, url, email-message, …). The crates are too immature to depend on. |
| CASE/UCO JSON-LD | no crate | — | **IN-HOUSE** serde → JSON-LD writer following the CASE 1.x / UCO 1.x ontology. Validate in CI with the CASE validation tooling (Python, dev-only), or with `oxigraph` 0.5.11 (MIT OR Apache) / `sophia` 0.10.0 for RDF parsing. `json-ld` 0.21.4 can expand and compact if needed. |

## 19. Testing, benchmarking, fuzzing, fixtures

| Need | Crate / tool | Version | Licence | Notes |
|---|---|---|---|---|
| Property tests | `proptest` | **1.12.0** | MIT OR Apache | In the workspace. `bolero` 0.13.7 unifies proptest and fuzzing if wanted. |
| Snapshots | `insta` (+ `cargo-insta`) | **1.49.0** | Apache-2.0 | Parser outputs as YAML/JSON snapshots; redact volatile fields. |
| Parametrised tests | `rstest` | **0.27.0** | MIT OR Apache | In the workspace. `test-case` 3.4.0 is an alternative. |
| Benchmarks | `criterion` **0.8.2** (criterion-rs org, 2026-02) / `divan` **0.1.21** (last release 2025-04, quiet) | MIT OR Apache | Use `criterion` for CI-tracked parsing/hashing throughput and `divan` for quick micro-benchmarks. |
| Test runner | `cargo-nextest` | **0.9.148** | Apache OR MIT | Per-test process isolation helps with parser panics and timeouts (`slow-timeout`). |
| Fuzzing | `cargo-fuzz` **0.13.2** + `libfuzzer-sys` **0.4.13** + `arbitrary` **1.5.0** | MIT OR Apache | One fuzz target per parser (image, volume, FS, artefact). Requires nightly for `cargo fuzz` (the local nightly 1.99 is available). |
| Mutation testing | `cargo-mutants` | **27.1.0** | MIT | Optional, for parsers. |
| GUI tests | `egui_kittest` | **0.36.2** | MIT OR Apache | See §1. |
| FS/image fixture writers (pure Rust) | `fatfs` **0.3.6** (FAT), `mbrman` **0.6.1**, `gptman` **3.1.1**, `hadris-iso`/`hadris-udf`/`hadris-fat` **2.5.0**, `fstool` **0.4.35** (ext2/3/4, FAT, exFAT, NTFS, XFS, HFS+, MBR/GPT), `arcbox-ext4` 0.1.2, `ewf-image` 0.6.0 (E01 writer), `outlook-pst-rw` 1.2.13 (PST writer), `lnk` 0.6.4, `cfb` 0.15.0, `rusty-leveldb` 4.0.1, `rust_xlsxwriter` | permissive | Generate small fixtures at build or test time (`pm-testkit`). **Commit** the bigger ones with SHA-256 manifests so tests do not depend on writer-crate behaviour changes. |
| Host tools present on this Mac (verified) | `hdiutil` (DMG UDRW/UDZO/UDBZ/ULFO/ULMO, sparseimage, sparsebundle; HFS+, APFS, FAT, exFAT; `makehybrid` for ISO/Joliet/UDF), `newfs_msdos`, `newfs_exfat`, `newfs_hfs`, `newfs_apfs`, `newfs_udf`, `mkfile`, **`qemu-img`** (Homebrew qemu 10.1.0: raw ↔ qcow2/vmdk/vpc(VHD)/vhdx/vdi), **`7zz`** 25.01, `sqlite3`, **Docker** | — | Not present: `mke2fs`, `mkfs.xfs`, `mkfs.btrfs`, `mkntfs`, `ewfacquire`, Sleuth Kit. **Run them in a Docker image** (Debian: `e2fsprogs xfsprogs btrfs-progs ntfs-3g dosfstools exfatprogs libewf-tools sleuthkit`). Use them to build Linux/NTFS/E01 fixtures, and run TSK `fls/istat/icat` as a **differential oracle**. Store generator scripts in `pm-testkit/fixtures/gen/`. |

---

## 20. Recommended Cargo dependency set

Exact versions as of 2026-10-14. Add them to `[workspace.dependencies]` as needed. This list has no
GPL/AGPL/LGPL entries, and **everything on the evidence path is pure Rust**. The exceptions are SQLite
(rusqlite `bundled`) as the case DB engine, `blake3`'s optional C/asm SIMD, and the optional `ffmpeg-sidecar`
feature.

```toml
[workspace.dependencies]
# --- GUI ---
egui            = "0.36.2"
eframe          = { version = "0.36.2", default-features = false, features = ["wgpu", "accesskit", "default_fonts", "persistence", "wayland", "x11"] }
egui_extras     = { version = "0.36.2", default-features = false, features = ["image", "svg"] }
egui_dock       = { version = "0.21.1", features = ["serde"] }
egui_table      = "0.10.0"
egui_plot       = "0.37.0"
walkers         = { version = "0.61.0", features = ["pmtiles"] }
egui_graphs     = "0.32.0"
petgraph        = "0.8.3"
egui-phosphor   = "0.14.0"
rfd             = "0.17.2"
image           = { version = "0.25.10", default-features = false, features = ["rayon", "jpeg", "png", "gif", "webp", "tiff", "bmp", "ico", "qoi", "tga"] }
jxl-oxide       = "0.12.6"
fast_image_resize = "6.1.0"
hayro           = "0.8.0"
html2text       = "0.17.3"
ammonia         = "4.2.3"
symphonia       = { version = "0.6.1", features = ["all"] }
rodio           = "0.23.0"
ffmpeg-sidecar  = { version = "2.6.0", optional = true }   # in the member crate, behind a feature

# --- Evidence parsing helpers (pure Rust) ---
notatin         = "1.0.1"
evtx            = "0.12.3"
lnk             = "0.6.4"
cfb             = "0.15.0"
plist           = "1.10.1"
mail-parser     = "0.11.9"
outlook-pst     = "1.2.0"
calcard         = "0.3.14"
prost           = "0.14.4"
snap            = "1.1.2"
lzxd            = "0.2.7"
lzfse_rust      = "0.2.1"
nom-exif        = "3.8.0"
kamadak-exif    = "0.6.1"
lofty           = "0.25.4"
file-format     = { version = "0.29.0", features = ["reader"] }
infer           = "0.22.0"

# --- Documents / text ---
pdf_oxide       = "0.3.78"
pdf-extract     = "0.12.1"
office_oxide    = "0.1.14"
calamine        = "0.36.1"
rtf-parser      = "0.4.3"
quick-xml       = "0.42.0"
chardetng       = "1.0.0"
encoding_rs     = "0.8.42"
encoding_rs_io  = "0.1.8"
whatlang        = "0.18.0"
ocrs            = { version = "0.13.1", optional = true }
rten            = { version = "0.26", optional = true }      # must match ocrs 0.13.1 (rten ^0.26)

# --- Archives / compression ---
zip             = { version = "9.0.3", default-features = false, features = ["aes-crypto", "deflate-flate2-zlib-rs", "deflate64", "bzip2", "lzma", "xz", "ppmd", "time"] }
sevenz-rust2    = "0.24.0"
rars            = "0.10.2"
tar             = "0.4.46"
flate2          = { version = "1.1.10", default-features = false, features = ["zlib-rs"] }
bzip2           = "0.6.1"
lzma-rust2      = "0.22.0"
ruzstd          = "0.9.1"
lz4_flex        = "0.14.0"
cab             = "0.6.0"

# --- Hashing ---
digest          = "0.11.3"
md-5            = "0.11.0"
sha1            = "0.11.0"
sha2            = "0.11.1"
blake3          = { version = "1.8.7", features = ["rayon"] }
tlsh2           = "1.1.0"
xxhash-rust     = { version = "0.8.19", features = ["xxh3"] }

# --- YARA / search ---
yara-x          = "1.21.0"
tantivy         = { version = "0.26.2", default-features = false, features = ["mmap", "stopwords", "lz4-compression", "stemmer"] }
regex           = "1.13.1"
regex-automata  = "0.4.18"
aho-corasick    = "1.1.5"
daachorse       = "5.0.0"
memchr          = "2.8.3"

# --- Database ---
rusqlite        = { version = "0.40.2", features = ["bundled", "blob", "backup", "functions", "hooks", "limits", "window", "column_decltype"] }  # 0.40 has no serde_json/uuid features
r2d2            = "0.8.10"
r2d2_sqlite     = "0.35.0"
postgres        = { version = "0.19.14", features = ["with-uuid-1", "with-serde_json-1"] }
tokio-postgres  = { version = "0.7.18", features = ["with-uuid-1", "with-serde_json-1"] }
deadpool-postgres = "0.14.2"
sea-query       = "1.0.2"
refinery        = { version = "0.10.0", features = ["rusqlite", "postgres"] }

# --- Infra (already partly present) ---
rayon           = "1.12.0"
crossbeam-channel = "0.5.17"
tokio           = { version = "1.53.3", features = ["rt-multi-thread", "macros", "sync", "time", "net"] }
tracing         = "0.1.44"
tracing-subscriber = { version = "0.3.23", features = ["env-filter", "json"] }
thiserror       = "2.0.21"
anyhow          = "1.0.104"
serde           = { version = "1.0.229", features = ["derive"] }
serde_json      = "1.0.154"
directories     = "6.0.0"
notify          = "8.2.0"
memmap2         = "0.9.11"
parking_lot     = "0.12.5"
dashmap         = "6.2.1"
moka            = { version = "0.12.16", features = ["sync"] }
jiff            = "0.2.38"
uuid            = { version = "1.28.0", features = ["v4", "v7", "serde"] }
zerocopy        = { version = "0.8.63", features = ["derive"] }
bstr            = "1.13.1"
bytesize        = "2.7.0"
clap            = { version = "4.6.7", features = ["derive"] }

# --- Multi-user ---
async-nats      = "0.50.0"
tonic           = "0.14.6"
tonic-prost     = "0.14.6"
# [build-dependencies] tonic-prost-build = "0.14.6"

# --- Plugins ---
pyo3            = { version = "0.29.3", features = ["auto-initialize"] }   # move to 0.30 once published
wasmtime        = "45.0.3"      # matches yara-x 1.21.0's ^45.0.3 to avoid two wasmtimes; else "49.0.2"
wasmtime-wasi   = "45.0.3"

# --- Reports ---
rust_xlsxwriter = "0.99.1"
askama          = "0.16.1"
minijinja       = "3.0.0"
kml             = "0.14.0"
geojson         = "1.0.0"

# --- Testing (dev-dependencies) ---
proptest        = "1.12.0"
insta           = { version = "1.49.0", features = ["yaml", "json", "redactions"] }
rstest          = "0.27.0"
criterion       = "0.8.2"
divan           = "0.1.21"
arbitrary       = { version = "1.5.0", features = ["derive"] }
libfuzzer-sys   = "0.4.13"
egui_kittest    = { version = "0.36.2", features = ["wgpu", "snapshot", "eframe"] }
tempfile        = "3.27.0"
fatfs           = { version = "0.3.6", default-features = false, features = ["std", "alloc"] }
mbrman          = "0.6.1"
gptman          = "3.1.1"
hadris-iso      = "2.5.0"
hadris-udf      = "2.5.0"
fstool          = "=0.4.35"     # unstable API, so pin exactly
ewf-image       = "0.6.0"
outlook-pst-rw  = "1.2.13"
rusty-leveldb   = "4.0.1"
# Oracles (dev-only, never shipped): ewf = "0.4.10", ntfs = "0.4.0", mft = "0.7.0",
#   ext4-view = "1.0.0", usnrs = "0.2.1", fuzzyhash = "0.2.2", xpress-huffman = "0.1.3",
#   keramics-formats = "0.0.1" (or git), safari-binarycookies = "1.0.0"
```

Tools to install, not as dependencies: `cargo-nextest 0.9.148`, `cargo-insta 1.49.0`, `cargo-fuzz 0.13.2`,
`cargo-mutants 27.1.0`. Also `qemu-img` (present), `hdiutil` (present), `7zz` (present), and a Docker image with
e2fsprogs/xfsprogs/btrfs-progs/ntfs-3g/exfatprogs/libewf-tools/sleuthkit.

Points to check when wiring:
- Pin `wasmtime` to the yara-x major, or accept the duplication.
- Never add `sqlx`, `sea-orm`, or `sea-query-rusqlite`: they conflict on `libsqlite3-sys`.
- Turn off the C `zstd` paths in `zip` and `tantivy` (shown above).
- egui 0.37 will need `default_fonts` explicitly (already listed).

---

## 21. Components we must write in-house

Ordered roughly by dependency and priority. "(ref: X)" names the best reference or oracle.

**Images and volumes**
1. Raw/split-raw segment reader (done/started in `pm-image`).
2. **EWF E01/Ex01/L01 reader**, plus MD5/SHA-1 verification (ref: libyal EWF spec, `ewf`, keramics).
3. VMDK, VHD, VHDX, QCOW2 readers (ref: keramics; fixtures via qemu-img).
4. DMG/UDIF reader with ADC + LZVN decoders; sparseimage/sparsebundle (ref: keramics, `udif`).
5. AFF4 (later), VSS/volsnap (later), VDI (later).
6. **MBR/EBR, GPT, APM, BSD disklabel** parsers with forensic tolerance (gaps, overlaps, backup GPT).
7. LVM2 linear/striped mapping; **BitLocker / LUKS / FileVault / CoreStorage detection** (decryption later).

**Filesystems (all forensic, read-only)**

8. **NTFS**: MFT, attributes, data runs, LZNT1, WOF/compact compression, ADS, $I30 slack, deleted/orphan
   records, $Bitmap → unallocated, $LogFile, **$UsnJrnl**, $Secure, $ObjId (ref: `ntfs`, `mft`, TSK).
9. **FAT12/16/32** (in progress, `pm-fs-fat`) and **exFAT**, including deleted entries and LFN recovery.
10. **ext2/3/4** with deleted inode scan and jbd2 (ref: `ext4-view`, TSK).
11. **HFS+** and **APFS** (incl. snapshots, decmpfs zlib/LZVN/LZFSE).
12. **ISO9660/Joliet/Rock Ridge/El Torito**, **UDF** (or adopt `hadris-*` after audit).
13. XFS, Btrfs (or adopt `btrfs-disk`), **YAFFS2**: lower priority.
14. Unallocated-space and slack "virtual files", plus a file carver (Autopsy's PhotoRec equivalent:
    header/footer signature carving).

**Artefacts and decoders**

15. **MS-XCA LZ77+Huffman (Prefetch MAM, also used elsewhere) and plain LZ77 / LZNT1** decoders.
16. **Prefetch** (SCCA v17–v31).
17. **ESE/JET Blue** reader (SRUM, WebCache, Windows Search, Exchange, `catalog*.edb`).
18. **SQLite forensic recovery**: freelist pages, unallocated and free blocks in leaf pages, WAL frame
    history, rollback journals.
19. **LevelDB raw reader** (incl. deleted records) + Chrome **IndexedDB** key coding + **V8/Blink value
    deserialiser**.
20. Schemaless **protobuf** decoder; **NSKeyedArchiver** resolver on top of `plist`.
21. Shell items (PIDL), jump-list DestList, LNK in jump lists.
22. Recycle Bin `$I`/INFO2, **thumbcache/iconcache**, Safari `.binarycookies`, EVTX chunk carving.
23. OLE property sets (Summary/DocumentSummaryInformation); CFB orphan-stream recovery.
24. MBOX splitter; PST/OST gaps (**OST 4 KiB / compressed blocks**, deleted items).
25. **ssdeep/CTPH** implementation (licence-clean).
26. Forensic **file-type signatures** layered over `file-format`.

**Platform and product**

27. Storage trait with **rusqlite** and **postgres** backends; sea-query value binder for rusqlite;
    `COPY`-based bulk loader.
28. PG LISTEN/NOTIFY listener + resync protocol; advisory-lock helper; NATS subjects and JetStream job
    schema; tonic index-server protocol (`.proto`).
29. Plugin ABI: WIT world for WASM plugins and the Python bridge (out-of-process recommended).
30. Report writers: **STIX 2.1** subset, **CASE/UCO JSON-LD**, HTML templates (askama/minijinja), KML
    wrapper.
31. GUI glue:
    - Native pop-out manager over `egui_dock` (deferred viewports, re-dock, geometry persistence).
    - Evidence-backed `egui` image/bytes loader and tiled large-image textures.
    - Paged table delegate for `egui_table`.
    - Hex viewer, text viewer with encoding switch, timeline, and graph layout off-thread.
    - HTML→`LayoutJob` renderer on top of `html2text`.
32. Fixture generator scripts (pure-Rust writers + `hdiutil`/`qemu-img` + Docker mkfs/libewf) and
    differential-oracle harnesses (TSK, libyal tools).

---

## 22. Sources

- crates.io API (`/api/v1/crates/<name>`, `/<name>/<ver>/dependencies`, `/owners`) and `cargo info` / `cargo search`, run 2026-10-14.
- egui sources `egui-0.36.2/src/context.rs` and `egui_extras-0.36.2/src/table.rs`; [egui CHANGELOG](https://github.com/emilk/egui/blob/main/CHANGELOG.md); [egui viewport docs](https://docs.rs/egui/latest/egui/viewport/index.html); open egui issues via the GitHub search API (numbers cited inline).
- [egui_kittest docs](https://docs.rs/egui_kittest/latest/egui_kittest/), [egui_dock README and CHANGELOG](https://github.com/anhosh/egui_dock).
- [keramics](https://github.com/keramics/keramics) (repo tree of `keramics-formats/src`), [hayro README](https://github.com/LaurenzV/hayro), [rars README](https://github.com/bitplane/rars), [outlook-pst-rs](https://github.com/microsoft/outlook-pst-rs) (+ crate source), [fstool README](https://github.com/KarpelesLab/fstool), [pdf_oxide](https://github.com/yfedoseev/pdf_oxide), [office_oxide](https://github.com/yfedoseev/office_oxide), [rust-lzxpress](https://github.com/comaeio/rust-lzxpress), [PyO3 CHANGELOG](https://github.com/PyO3/pyo3/blob/main/CHANGELOG.md), [tantivy CHANGELOG](https://github.com/quickwit-oss/tantivy/blob/main/CHANGELOG.md).
- Local tool check (`command -v`) on this Mac: hdiutil, newfs_*, qemu-img (qemu 10.1.0), 7zz 25.01, docker present; mkfs.ext4/xfs/btrfs, mkntfs, ewfacquire, and TSK absent.

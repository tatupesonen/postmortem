# Architecture decision records

## ADR-001 — Pure-Rust evidence parsing
All image, volume, filesystem and artefact parsing is written in Rust (own
code or pure-Rust crates). No libtsk/libewf FFI. Rationale: memory safety on
hostile input, no C toolchain, simpler cross-platform builds. Consequence:
filesystems arrive incrementally and need extensive fixture/fuzz testing.

## ADR-002 — Own case schema, not TSK-compatible
postmortem uses its own case-database schema, designed around the queries the
UI and ingest actually run (covering indexes, integer enums, WAL, batched
writes). SQLite for single-user cases and PostgreSQL for multi-user, behind
one storage trait. An Autopsy-case importer can be added later.

## ADR-003 — Positional `ReadAt` as the universal byte-source abstraction
Every evidence layer implements `pm_core::io::ReadAt` (`&self` positional
reads, `Send + Sync`). This allows lock-free sharing between ingest threads
and composes images → volumes → files.

## ADR-004 — Panic-free parsing enforced by lints
`clippy::indexing_slicing` and `unwrap_used` are denied workspace-wide outside
tests. Parsers use `pm_core::Bytes` checked accessors. Where an index is
provably in range and the checked form hurts clarity, use
`#[expect(clippy::indexing_slicing, reason = "...")]`.

## ADR-005 — Metric (SI) units by default
Sizes are shown with SI prefixes (kB, MB, GB) by default (organisation
policy), with IEC available. Timestamps are ISO-8601, UTC by default, with a
configurable display time zone.

## ADR-006 — Local git checkpoints
The autonomous loop commits each finished, green slice to a local git
repository in `postmortem/`. It never pushes or adds remotes.

## ADR-007 — Database layer: rusqlite + postgres, no sqlx/sea-orm
SQLite access goes through `rusqlite` (bundled), PostgreSQL through
`postgres`/`tokio-postgres` (+ `deadpool-postgres`). Dialect-portable SQL is
built with `sea-query` where it helps, hand-written SQL elsewhere, and
migrations use `refinery` (both back ends). `sqlx`, `sea-orm` and
`sea-query-rusqlite` are banned: they pin conflicting `libsqlite3-sys`
versions (`links = "sqlite3"`). Inventory 07 §0.3 and §14.

## ADR-008 — GUI stack: eframe/egui 0.36, egui_dock, deferred viewports
UI is eframe 0.36.2 (wgpu) with `egui_dock` for the docked main layout and
**deferred viewports** for native pop-out windows. Our own code manages the
pop-outs, because egui_dock only tears tabs out *inside* the window.
Workarounds for known multi-viewport bugs (macOS focus/resize) are listed in
inventory 07 §1.1. Upgrade to 0.37 once released (it needs `default_fonts`
explicitly).

## ADR-009 — Third-party forensic parsers are oracles, not dependencies
The 2026 pure-Rust forensic crates (keramics, SecurityRonin, hadris, …) are
young and unvetted. They may be used as **dev-dependencies for differential
tests**, never on the evidence path, except well-established format crates
listed in inventory 07 §20 (e.g. `evtx`, `cfb`, `plist`, `mail-parser`,
`notatin`), each of which needs an audit note in this file before adoption.

## ADR-010 — Licence policy
The product is Apache-2.0. GPL/AGPL/LGPL crates are banned from shipped
binaries (watch-list in inventory 07 §0.5); MPL-2.0 is allowed unmodified.
Enforce with `cargo-deny` (`deny.toml`) when it lands in the gate.

## ADR-011 — wasmtime pinned to yara-x's major
`yara-x` 1.21 requires wasmtime ^45. The WASM plugin host uses the same major
to avoid shipping two wasmtime copies. Revisit when yara-x moves.

## ADR-012 — TSK parity through differential testing
Where Autopsy's behaviour comes from The Sleuth Kit (volume layout, FS trees,
names, orphans), we match TSK 4.15 output (`mmls`/`fls`/`istat`) on
generated images via `pm_testkit::tsk`. Where we recover *more* (full deleted
long names, orphan content), the differential test accepts the TSK form as
an alternative and the improvement is documented in the test.

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

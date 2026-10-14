# Progress

## Now

- Bootstrap: Autopsy surveys are being written to `docs/inventory/01..08`.
  Once they exist, write `docs/PLAN.md` (architecture, crate map, milestones,
  task checklist), then start Milestone 1.
- The test-parity survey (`docs/inventory/08-autopsy-tests.md`) still has to
  be produced if it is missing. Cover every *Test.java, test/ regression
  scripts and fixtures, ending in a parity checklist.

## Blockers

_None._

## Log

### 2026-10-14 — bootstrap
- Workspace skeleton: virtual manifest, pinned toolchain 1.97.1, workspace
  lints (pedantic + no-panic restrictions), clippy.toml test allowances.
- `pm-core`: `ReadAt` (Mem/File/Slice/Zero sources, Read+Seek cursor),
  bounds-checked `Bytes` decoding, UTF-16/C-string helpers, timestamp
  converters (FILETIME, WebKit, HFS, Cocoa, Unix s/ms/ns, DOS), SI/IEC size
  formatting. 34 tests (unit + proptest), gate green.

# Direction — the owner's standing decisions

Decisions and working rules given by the project owner, collected so any
session on any machine follows them without re-asking. Newest first
within each section. Architecture decisions with rationale live in
`DECISIONS.md` (ADRs); this file records *what the owner decided*.

## Goal

**Parity with Autopsy**, then beyond it (`BEYOND-AUTOPSY.md`), on huge
evidence. Continuously compare features and behaviour against the Autopsy
source (`../autopsy`, read-only), port its tests, and keep performance,
memory use, good design and a sane architecture in mind.

## Scope

| Decision | Date |
|---|---|
| **Multi-user** cases are in scope, on **PostgreSQL through a dialect/query layer** in `pm-casedb`; SQLite stays for single-user cases (ADR-024). Postgres tests use Docker/Podman with the official image. | 2026-10-18 / 2026-10-17 |
| **WebAssembly plugins** are in scope; built-in parsers are native Rust written against the same `pm-plugin-api` ("Native, same API", ADR-022). | 2026-10-17 |
| **External-tool runners are out** (aLEAPP/iLEAPP, Plaso, Volatility): port valuable parsers natively instead. **Cyber Triage** is out. | 2026-10-17 |
| An **MCP server** for AI assistants is in scope (`postmortem mcp`, ADR-021). | 2026-10-17 |
| The **UI redesign** (design import) is postponed: features first. | 2026-10-17 |

## Architecture

| Decision | Date |
|---|---|
| **Everything reads the case database** — timeline, gallery, discovery, summaries, searches, MCP, reports. No in-memory walks or indexes of the evidence for features (ADR-023). | 2026-10-18 |
| UI must never freeze on large cases: database and evidence work runs off the UI thread. | 2026-10-17 |

## Priorities

1. Architecture cleanup from the 2026-10-18 review (ADR-023): remove the
   in-memory evidence index and the GUI's second evidence open; case
   change bus; split `pm-ui/src/app.rs`; rename the `pm-casedb` query
   layer; separate UI thread pool; then the Postgres dialect layer
   (ADR-024).
2. **Analyst-workflow features** (`BEYOND-AUTOPSY.md` §8): multi-select
   in tables (Ctrl/Shift), hotkeys and a command palette, global quick
   search, pivot page, notes/findings, triage dashboard, saved views,
   review state, timeline noise reduction, IOC import.
3. Remaining Autopsy parity gaps (`PARITY.md`).

## Specific product decisions

| Decision | Date |
|---|---|
| **Data-source time zone:** keep the wizard default (examiner's zone), but after Recent Activity reads the registry, warn when the system's time zone differs and offer to switch. | 2026-10-18 |
| **Image Gallery:** identical pictures (same MD5) are shown once; a toggle shows duplicates. | 2026-10-18 |
| **OS Accounts** sit under Analysis Results in the tree. | 2026-10-17 |
| **Result tabs** like Autopsy's, so the examiner can switch between listings and searches. | 2026-10-17 |
| Viewers share one implementation (metadata, MIME, hashes in every viewer); no per-viewer duplication. | 2026-10-17 |

## Working rules

* **Commit and push** finished work straight to `master` and keep
  `parity-wip` in sync: `git push origin master master:parity-wip`. Work
  that is not pushed can be lost (it happened once).
* **Solo** work (no parallel agents) since 2026-10-18; use the whole CPU.
* **Build cadence:** `cargo check` / targeted clippy and the changed
  crate's tests while iterating; the full gate (`scripts/gate.sh`, output
  to a log file, never piped) after a big feature, before pushing.
* **Field-test on real images** (`~/dfir` on the owner's Linux machine:
  DFIR Madness "Stolen Szechuan Sauce" DESKTOP-SDN1RPT E01, Greg Schardt's
  Dell Latitude CPi E01). Log what was found and the gaps in
  `BEYOND-AUTOPSY.md`.
* **Existing cases** made by older builds may be upgraded/re-analysed
  (e.g. re-run Recent Activity after a parser fix), owner-approved
  2026-10-18.
* Reference sources (Eric Zimmerman's tools, the reference tools, Tika) may be
  cloned **outside** the repo (`../reference/`).
* **Secrets:** credentials the owner shares (e.g. for package installs)
  are used once and never written to files, commits or notes.
* Use metric units.

## Environment notes (owner's Linux VM)

* The GUI in the VMware VM needs `LIBGL_ALWAYS_SOFTWARE=1` or
  `WGPU_BACKEND=vulkan`; do not add an automatic adapter selector.
* Watch disk use: debug `target/` can exceed 100 GB; `cargo clean` when
  needed, and keep test data out of `/tmp` leaks.
* The installed binary for MCP is `~/.local/bin/postmortem`; the global
  Claude Code MCP servers `postmortem-greg` and `postmortem-desktop` use
  it (`postmortem mcp --case <dir>`).

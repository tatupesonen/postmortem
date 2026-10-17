# Autonomous loop protocol

postmortem is developed by a self-paced agent loop. Every iteration starts from
zero conversational memory and must be able to continue purely from the files
in this repository. This document is the contract each iteration follows.

## State files (source of truth)

| File | Purpose |
|------|---------|
| `docs/DIRECTION.md` | The owner's standing decisions, priorities and working rules. Read first. |
| `docs/LOOP.md` | This protocol. Change only when the process itself needs to change. |
| `docs/PLAN.md` | Architecture, crate map, milestones and the task checklist (`- [ ]` / `- [x]`). |
| `docs/PROGRESS.md` | `## Now` (current task + next steps), `## Blockers`, and an append-only `## Log` (newest first). |
| `docs/DECISIONS.md` | Architecture decision records (ADR-NNN). Append; never silently rewrite. |
| `docs/inventory/*.md` | Feature inventory of Autopsy, by area. The parity reference. |

The Autopsy source at `../autopsy` is **read-only reference**.
Never modify it.

## One iteration

1. **Orient** (keep it cheap): read `DIRECTION.md` (owner decisions), then
   `PROGRESS.md` `## Now` and `## Blockers`, then only the `PLAN.md`
   milestone that holds the current task. Run
   `git log --oneline -5` to confirm where we are.
2. **Baseline**: make sure the last pushed commit passed the quality gate
   (below); if it is red, fixing it *is* the task for this iteration.
3. **Pick work**: the current task in `## Now`, or else the first unchecked
   task in the earliest unfinished milestone. Work on a vertical slice that can
   be finished, tested and committed within the iteration. Split tasks in
   `PLAN.md` if they are too large.
4. **Research**: read the relevant inventory section, then the specific Autopsy
   Java sources it cites if behaviour is unclear. Port *behaviour*, not Java
   structure.
5. **Implement** following the engineering rules below, tests included.
6. **Check while iterating, gate when done** (owner rule 2026-10-17):
   use `cargo check`, targeted `cargo clippy -p <crate>` and the changed
   crates' tests while working; run the full gate after a big feature,
   before pushing it.
7. **Record**: tick boxes in `PLAN.md`, update `## Now` with the precise next
   step, prepend a dated entry to `## Log` (what was done, what was learned,
   test counts), add an ADR for any non-obvious decision.
8. **Commit and push**: stage the files you changed explicitly (never
   editor swap files), commit with a message that says what changed and
   why, ending with the Co-Authored-By trailer, then
   `git push origin master master:parity-wip` (owner decision: finished
   work goes straight to `master`). Push a big feature only after a green
   full gate. Never add remotes.
9. **Continue**: if there is remaining work and context budget, go back to 3.
   Otherwise end the turn; the loop wakeup re-enters at step 1.

## Quality gate

Run `scripts/gate.sh` from the repository root (fmt check, clippy
with `-D warnings`, all tests), writing its output to a log file:
`scripts/gate.sh > gate.log 2>&1; echo $?`. **Never pipe the gate**
(`gate.sh | tail` returns tail's exit status and hides a red gate). UI
flow tests occasionally time out under full-gate load; rerun the single
test before treating it as a failure.

UI snapshot tests (egui_kittest) and slow tests (large fixtures, fuzz corpora)
may be gated behind `--features slow-tests`/`#[ignore]` and run at milestone
ends.

## Engineering rules

- **Workspace of focused crates** (see `PLAN.md` crate map). Library crates
  never depend on UI crates. Parsers depend only on `pm-core` primitives.
- **No `unsafe`** outside a crate explicitly allowed in `PLAN.md`
  (`#![forbid(unsafe_code)]` everywhere else).
- **No panics on untrusted input.** Every parser treats evidence as hostile:
  bounds-checked reads, `checked_*` arithmetic, recursion/size limits, fuzz
  targets for each binary format parser.
- **Errors**: `thiserror` in libraries, `anyhow` only in binaries/tests.
- **Logging**: `tracing`. No `println!` in libraries.
- **Performance (ADR-014: tens of TB, 100s of millions of files)**: no
  O(files) RAM structures; indexed queries only; keyset pagination;
  benchmark new hot paths against the scale budgets. Stream; never read whole evidence files into memory; batch
  DB writes in transactions; parallelise with rayon at the file level; UI never
  blocks on IO (background jobs + channels, virtualised tables).
- **Pure Rust** for parsing. A C dependency needs an ADR.
- **Metric units** and ISO-8601/UTC-first timestamps in UI and reports.

## Testing rules

- **Test parity with Autopsy**: before finishing a feature, check
  `docs/inventory/08-autopsy-tests.md` (and the Autopsy test sources it cites)
  for tests covering that area; port their cases and tick the checklist there.
- **Golden regression files**: JSON goldens (e.g. `crates/pm-mcp/tests/golden/`)
  are regenerated with `UPDATE_GOLDEN=1` and the diff reviewed before
  committing — never blindly. A whole-case golden dump over the fixture
  images (like Autopsy's `test/` scripts) is still to be built.
- Integration-test crates (`tests/*.rs`) start with
  `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::print_stderr, reason = "test code")]`
  because helper functions there are not covered by clippy.toml's test allowances.
- **Differential tests against TSK** (`pm_testkit::tsk`, needs
  `apt install sleuthkit` or `brew install sleuthkit`): volume and filesystem output must match
  `mmls`/`fls` on generated images, except for documented improvements.
- Every public behaviour gets a test. Unit tests sit next to code; cross-crate
  behaviour goes in `tests/`.
- Parsers: hand-built minimal fixtures generated **in Rust test code** where
  possible (builders in `pm-testkit`), plus small checked-in images under
  `fixtures/` created by documented scripts (`hdiutil`, `qemu-img`, …).
  Record fixture provenance in `fixtures/README.md`.
- Property tests (`proptest`) for parsers/codecs ("never panics", round-trips).
- Snapshot/golden files for reports and complex structured output.
- UI: `egui_kittest` interaction tests for key flows.
- Fuzz targets (`cargo fuzz`, in `fuzz/`) for every binary parser; a short
  smoke run at milestone ends.
- Every fuzz crash/timeout is fixed, its input saved under
  `fuzz/regressions/<target>/`, and replayed by a normal `cargo test`.
- Benchmarks for hot paths as `#[ignore]` release tests (commands and
  results in `PERF.md`); real-image runs with `postmortem ingest`.

## Stopping

The loop stops only when every milestone in `PLAN.md` is complete, or when a
blocker needs a human decision. In that case, record it under `## Blockers`,
then continue with any other unblocked work. Stop the loop only if nothing is
unblocked.

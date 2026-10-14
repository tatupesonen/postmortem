# Autonomous loop protocol

postmortem is developed by a self-paced agent loop. Every iteration starts from
zero conversational memory and must be able to continue purely from the files
in this repository. This document is the contract each iteration follows.

## State files (source of truth)

| File | Purpose |
|------|---------|
| `docs/LOOP.md` | This protocol. Change only when the process itself needs to change. |
| `docs/PLAN.md` | Architecture, crate map, milestones and the task checklist (`- [ ]` / `- [x]`). |
| `docs/PROGRESS.md` | `## Now` (current task + next steps), `## Blockers`, and an append-only `## Log` (newest first). |
| `docs/DECISIONS.md` | Architecture decision records (ADR-NNN). Append; never silently rewrite. |
| `docs/inventory/*.md` | Feature inventory of Autopsy, by area. The parity reference. |

The Autopsy source at `/Users/tatu/slop/autopsy` is **read-only reference**.
Never modify it.

## One iteration

1. **Orient** (keep it cheap): read `PROGRESS.md` `## Now` and `## Blockers`,
   then only the `PLAN.md` milestone that holds the current task. Run
   `git -C /Users/tatu/slop/postmortem log --oneline -5` to confirm where we are.
2. **Baseline**: run the quality gate (below). If it is red, fixing it *is* the
   task for this iteration.
3. **Pick work**: the current task in `## Now`, or else the first unchecked
   task in the earliest unfinished milestone. Work on a vertical slice that can
   be finished, tested and committed within the iteration. Split tasks in
   `PLAN.md` if they are too large.
4. **Research**: read the relevant inventory section, then the specific Autopsy
   Java sources it cites if behaviour is unclear. Port *behaviour*, not Java
   structure.
5. **Implement** following the engineering rules below, tests included.
6. **Gate**: the quality gate must pass.
7. **Record**: tick boxes in `PLAN.md`, update `## Now` with the precise next
   step, prepend a dated entry to `## Log` (what was done, what was learned,
   test counts), add an ADR for any non-obvious decision.
8. **Commit**: `scripts/gate.sh && git add -A && git commit` in `postmortem/` with a conventional
   message (`feat(fs-ntfs): …`) ending with the Co-Authored-By trailer. Local
   commits only — never push, never add remotes.
9. **Continue**: if there is remaining work and context budget, go back to 3.
   Otherwise end the turn; the loop wakeup re-enters at step 1.

## Quality gate

Run `scripts/gate.sh` from `/Users/tatu/slop/postmortem` (fmt check, clippy
with `-D warnings`, all tests). **Commit only as `scripts/gate.sh && git commit …`**
so that a red gate can never be committed. **Never pipe the gate** (`gate.sh | tail` returns tail's exit
status and lets a red gate through).

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
- **Performance**: stream; never read whole evidence files into memory; batch
  DB writes in transactions; parallelise with rayon at the file level; UI never
  blocks on IO (background jobs + channels, virtualised tables).
- **Pure Rust** for parsing. A C dependency needs an ADR.
- **Metric units** and ISO-8601/UTC-first timestamps in UI and reports.

## Testing rules

- **Test parity with Autopsy**: before finishing a feature, check
  `docs/inventory/08-autopsy-tests.md` (and the Autopsy test sources it cites)
  for tests covering that area; port their cases and tick the checklist there.
- **Golden regression harness**: like Autopsy's `test/` scripts, ingest the
  fixture images end-to-end and compare a normalised case-DB dump against a
  checked-in golden file (`insta`). Update goldens deliberately, never blindly.
- Integration-test crates (`tests/*.rs`) start with
  `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::print_stderr, reason = "test code")]`
  because helper functions there are not covered by clippy.toml's test allowances.
- **Differential tests against TSK** (`pm_testkit::tsk`, needs
  `brew install sleuthkit`): volume and filesystem output must match
  `mmls`/`fls` on generated images, except for documented improvements.
- Every public behaviour gets a test. Unit tests sit next to code; cross-crate
  behaviour goes in `tests/`.
- Parsers: hand-built minimal fixtures generated **in Rust test code** where
  possible (builders in `pm-testkit`), plus small checked-in images under
  `fixtures/` created by documented scripts (`hdiutil`, `qemu-img`, …).
  Record fixture provenance in `fixtures/README.md`.
- Property tests (`proptest`) for parsers/codecs ("never panics", round-trips).
- Snapshot tests (`insta`) for reports and complex structured output.
- UI: `egui_kittest` interaction tests for key flows.
- Fuzz targets (`cargo fuzz`, in `fuzz/`) for every binary parser; a short
  smoke run at milestone ends.
- Every fuzz crash/timeout is fixed, its input saved under
  `fuzz/regressions/<target>/`, and replayed by a normal `cargo test`.
- Benchmarks (`divan`/`criterion`) for hot paths (hashing, NTFS MFT walk,
  keyword search, table queries).

## Stopping

The loop stops only when every milestone in `PLAN.md` is complete, or when a
blocker needs a human decision. In that case, record it under `## Blockers`,
then continue with any other unblocked work. Stop the loop only if nothing is
unblocked.

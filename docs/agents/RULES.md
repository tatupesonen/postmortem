# Rules for parallel implementation agents

Several agents work at once, each in its own git worktree, and the
coordinator merges their branches into `master`. Follow these rules so
merges are clean and the result is shippable.

## Read first
`docs/LOOP.md` (engineering + testing rules, quality gate), `docs/PROGRESS.md`
(`## Now`), `docs/DECISIONS.md` (ADR-009 prefer existing crates, ADR-013
analysers in Rust, ADR-014 scale, ADR-015 ingest framework), the
`docs/inventory/*.md` sections for your track, and `docs/inventory/08-autopsy-tests.md`.
Autopsy's Java source at `/Users/tatu/slop/autopsy` is the parity reference —
read-only, never modify it. Port behaviour (artifact types, attribute names,
scores, messages, skip rules, defaults), not Java structure.

## Build hygiene (shared 8-core / 17 GB machine)
- Prefix every cargo command with `CARGO_BUILD_JOBS=2`.
- During development build/test only your crates (`cargo test -p <crate>`).
- Before each commit run the full gate exactly as
  `CARGO_BUILD_JOBS=2 scripts/gate.sh && git add -A && git commit -m "…"`.
  Never pipe the gate. The gate prints nothing for many minutes; wait.
- Do not run `cargo clean` on anything outside your worktree.

## Ownership (avoid merge conflicts)
- **Never edit `crates/pm-ui`** (UI integration is done separately).
- Put new functionality in **new crates** where it is a reusable parser or
  engine (e.g. `pm-filetype`, `pm-registry`, `pm-email`), and the ingest
  module glue in `crates/pm-modules/src/<module>.rs` (or a folder).
- Shared files — keep edits minimal and append-only:
  - `Cargo.toml`: add your crates to `[workspace.dependencies]` / new
    third-party deps (exact versions, at least two weeks old).
  - `crates/pm-modules/src/lib.rs`: add `pub mod` lines and insert your
    factories into `core_modules()` in Autopsy's pipeline order.
  - `crates/pm-modules/Cargo.toml`: add your dependencies.
- `crates/pm-casedb` **schema** changes: only the derived-files/embedded
  track may add a migration. Others needing data must use artifacts and
  attributes (the catalogue in `pm-casedb/src/catalog.rs`; add missing
  standard TSK types there) or add read/query helpers in a **new file**
  `crates/pm-casedb/src/<track>.rs` (`impl CaseDb { … }`).
- `crates/pm-ingest`: only the derived-files/embedded track changes it.
- Docs: write your status to `docs/agents/<track>.md` (what is done, how it
  is tested, known gaps vs Autopsy, follow-ups). Do not rewrite
  `docs/PROGRESS.md` `## Now`; you may tick your boxes in `docs/PLAN.md`
  and `docs/inventory/08-autopsy-tests.md`.

## Quality bar
- Rust best practices: no `unsafe`, no panics on evidence input
  (bounds-checked, `checked_*`, size/recursion limits), `thiserror` errors,
  `tracing` logs, small focused modules, documented public items.
- Performance (ADR-014): stream content in chunks, never read whole large
  files, no O(files) memory, indexed DB queries with keyset paging, batch
  writes, cancellation polling (`ctx.is_cancelled()`) in long loops,
  bounded caches. Add a benchmark (`#[ignore]` test or divan) for hot paths
  and record numbers in `docs/PERF.md`.
- Tests for every behaviour: unit tests, integration tests in `tests/`
  (fixtures generated in Rust with `pm-testkit` builders or small
  documented files under `fixtures/`), `proptest` "never panics" for
  parsers, a `cargo-fuzz` target per binary parser (in `fuzz/`), and
  end-to-end module tests that run ingest on a case (see
  `crates/pm-modules/tests/common/mod.rs`). Port Autopsy's own test cases.
- A real Windows XP image is available read-only at
  `~/Downloads/4Dell Latitude CPi.E01` (+ `.E02`) for opt-in real-data tests
  (`PM_REAL_DISK` pattern in `crates/pm-case/tests/case.rs`); never
  commit it or depend on it in the default test run.

## Commits
Conventional messages (`feat(email): …`), several small commits are
better than one huge one, each ending with
`Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.
Local commits only — never push, never add remotes.

## Final report
List commits, what each feature covers, test counts, benchmark numbers,
and remaining gaps vs Autopsy (also in `docs/agents/<track>.md`).

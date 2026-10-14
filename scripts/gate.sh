#!/usr/bin/env bash
# Quality gate (see docs/LOOP.md). Exits non-zero on the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --quiet -- -D warnings
cargo test --workspace --all-features --quiet 2>&1 | tee /tmp/pm-gate-test.log | grep -E '^test result|panicked|FAILED' \
  | awk '/test result/{p+=$4; f+=$6} /panicked|FAILED/{print} END {printf "tests: %d passed, %d failed\n", p, f; exit (f>0)}'

#!/usr/bin/env bash
# Runs every fuzz target briefly (default 30 s each). Usage: scripts/fuzz-smoke.sh [seconds]
set -euo pipefail
secs="${1:-30}"
cd "$(dirname "$0")/../fuzz"
for t in $(cargo +nightly fuzz list); do
    echo "== fuzz $t (${secs}s)"
    mkdir -p "corpus/$t"
    [ -d "seeds/$t" ] && cp -n seeds/"$t"/* "corpus/$t/" 2>/dev/null || true
    cargo +nightly fuzz run "$t" -- -max_total_time="$secs" -max_len=1048576 -rss_limit_mb=4096
done

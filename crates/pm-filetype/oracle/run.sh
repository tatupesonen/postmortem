#!/usr/bin/env bash
# Opt-in differential test of pm-filetype against real Apache Tika 3.0.0
# (never part of the default test run; needs Java 11+ and tika-app).
#
#   crates/pm-filetype/oracle/run.sh OUT_DIR ROOT...
#
# Builds a stratified file list from the ROOT directories (corpus.py),
# runs Tika (TikaOracle.java, launched as a single source file) and
# pm-filetype (examples/tika_oracle.rs) over it and prints the agreement
# for Tika's two passes and for Autopsy's combined answer (compare.py).
#
# TIKA_JAR defaults to /home/tatu/slop/reference/tika-app/tika-app-3.0.0.jar
# (Maven Central: org/apache/tika/tika-app/3.0.0/tika-app-3.0.0.jar).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
out="$1"
shift
jar="${TIKA_JAR:-/home/tatu/slop/reference/tika-app/tika-app-3.0.0.jar}"
mkdir -p "$out"
python3 -I "$here/corpus.py" "$@" > "$out/paths.txt"
(cd "$root" && cargo build --release -p pm-filetype --example tika_oracle)
java -cp "$jar" "$here/TikaOracle.java" < "$out/paths.txt" > "$out/tika.tsv" 2>/dev/null
"$root/target/release/examples/tika_oracle" < "$out/paths.txt" > "$out/rust.tsv"
python3 -I "$here/compare.py" "$out/tika.tsv" "$out/rust.tsv"

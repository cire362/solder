#!/usr/bin/env bash
# Measures the three numbers the website promises, on a release build.
#   scripts/bench.sh [file]
# Cold start is the median of 5 launches (first launch discarded as disk-cold).
set -euo pipefail
cd "$(dirname "$0")/.."
file="${1:-crates/solder/src/editor.rs}"
cargo build --release -p solder --quiet
bin="${CARGO_TARGET_DIR:-target}/release/solder"

SOLDER_BENCH_STARTUP=1 "$bin" "$file" >/dev/null
starts=()
for _ in 1 2 3 4 5; do
  starts+=("$(SOLDER_BENCH_STARTUP=1 "$bin" "$file" | sed 's/first_frame_ms=//')")
done
median=$(printf '%s\n' "${starts[@]}" | sort -n | sed -n 3p)
echo "cold_start_ms=$median (runs: ${starts[*]})"
SOLDER_BENCH_TYPING=1 "$bin" "$file"

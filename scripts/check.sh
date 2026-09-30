#!/usr/bin/env bash
# Everything CI checks, in the order that fails fastest.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== website: lint"
npm run lint --silent
echo "== website: types"
npm run typecheck --silent

echo "== editor: format"
(cd editor && cargo fmt --all --check)
echo "== editor: clippy"
(cd editor && cargo clippy --workspace --all-targets --profile ci --locked --quiet -- -D warnings)
echo "== editor: test build"
(cd editor && cargo test --workspace --profile ci --locked --no-run --quiet)
echo "== editor: tests"
(cd editor && cargo test --workspace --profile ci --locked --quiet)

echo "All checks passed."

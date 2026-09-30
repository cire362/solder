#!/usr/bin/env bash
# Everything CI checks, in the order that fails fastest.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== website: lint"
npm run lint --silent
echo "== website: types"
npx tsc --noEmit

echo "== editor: format"
(cd editor && cargo fmt --all --check)
echo "== editor: clippy"
(cd editor && cargo clippy --workspace --all-targets --quiet -- -D warnings)
echo "== editor: tests"
(cd editor && cargo test --workspace --quiet)

echo "All checks passed."

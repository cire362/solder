#!/usr/bin/env bash
# The example plugins and what they are written against.
#
# TypeScript: each example must compile against editor/plugins/solder.d.ts,
# and its committed plugin.js must be what the compiler writes, since that
# file is what the editor's tests run.
#
# Go, where it is installed: the package is formatted, vetted and tested,
# and each example builds for WASI.
set -euo pipefail
cd "$(dirname "$0")/.."

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
for config in editor/plugins/*/tsconfig.json; do
  dir="$(dirname "$config")"
  npx tsc -p "$dir" --outDir "$out"
  if ! diff -q "$out/plugin.js" "$dir/plugin.js" >/dev/null; then
    echo "$dir/plugin.js is out of date: run npx tsc -p $dir" >&2
    exit 1
  fi
done

if command -v go >/dev/null; then
  unformatted="$(gofmt -l editor/plugins editor/crates/plugin/tests/fixtures)"
  if [ -n "$unformatted" ]; then
    echo "not gofmt-ed: $unformatted" >&2
    exit 1
  fi
  (cd editor/plugins/go && go vet ./... && go test ./...)
  for mod in editor/plugins/*/go.mod; do
    dir="$(dirname "$mod")"
    [ "$dir" = editor/plugins/go ] && continue
    (cd "$dir" && GOOS=wasip1 GOARCH=wasm go vet ./... && GOOS=wasip1 GOARCH=wasm go build -o "$out/plugin.wasm" .)
  done
else
  echo "go is not installed: the Go plugins were not checked"
fi

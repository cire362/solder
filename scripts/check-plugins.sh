#!/usr/bin/env bash
# The example plugins written in TypeScript: each must compile against
# editor/plugins/solder.d.ts, and its committed plugin.js must be what the
# compiler writes, since that file is what the editor's tests run.
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

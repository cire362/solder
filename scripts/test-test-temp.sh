#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
parent=$(mktemp -d "${TMPDIR:-/tmp}/solder-temp-check.XXXXXX")
trap 'rm -rf -- "$parent"' EXIT
for result in 0 7; do
  status=0
  TMPDIR="$parent" bash scripts/with-test-temp.sh bash -c '
    test "$TMPDIR" = "$TEMP" && test "$TEMP" = "$TMP"
    mkdir -p "$TMPDIR/fixture/nested"
    printf content > "$TMPDIR/fixture/nested/file"
    printf "%s" "$TMPDIR" > "$1/path"
    exit "$2"
  ' bash "$parent" "$result" || status=$?
  test "$status" = "$result"
  test ! -e "$(cat "$parent/path")"
done

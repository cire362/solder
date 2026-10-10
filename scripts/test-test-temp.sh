#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
test_parent=${TMPDIR:-/tmp}
parent=$(mktemp -d "${test_parent%/}/solder-temp-check.XXXXXX")
trap 'rm -rf -- "$parent"' EXIT
for result in 0 7; do
  status=0
  TMPDIR="$parent/" bash scripts/with-test-temp.sh bash -c '
    case "$TMPDIR" in *//*) exit 5 ;; esac
    test "$TMPDIR" = "$TEMP" && test "$TEMP" = "$TMP"
    mkdir -p "$TMPDIR/fixture/nested"
    printf content > "$TMPDIR/fixture/nested/file"
    printf "%s" "$TMPDIR" > "$1/path"
    exit "$2"
  ' bash "$parent" "$result" || status=$?
  test "$status" = "$result"
  test ! -e "$(cat "$parent/path")"
done

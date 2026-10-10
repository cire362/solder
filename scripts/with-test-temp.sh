#!/usr/bin/env bash
# One root per test run: fixtures are removed on success and on failure,
# including tests that panic before their own cleanup is reached.
set -euo pipefail
test_root=$(mktemp -d "${TMPDIR:-/tmp}/solder-tests.XXXXXX")
trap 'rm -rf -- "$test_root"' EXIT
export TMPDIR="$test_root" TEMP="$test_root" TMP="$test_root"
"$@"

#!/usr/bin/env bash
set -euo pipefail

runner="$1"
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
fixture=$(realpath -e -- "$fixture")
mkdir -m 700 "$fixture/safe"
mkdir -m 777 "$fixture/unsafe"
mkdir -m 700 "$fixture/unsafe/child"
ln -s "$fixture/safe" "$fixture/alias"

check_runner() {
  local candidate="$1"
  local expected="$2"

  (
    umask 000
    NIX_BUILD_TOP="$candidate" TMPDIR="$candidate" bash "$runner" bash -euc '
      [[ "$(umask)" == 0077 ]]
      [[ "$(stat --format=%a -- "$TMPDIR")" == 700 ]]
      [[ "$(dirname -- "$TMPDIR")" == "$1" ]]
      [[ "$2" == "literal argument with spaces" ]]
      mkdir "$TMPDIR/child"
      [[ "$(stat --format=%a -- "$TMPDIR/child")" == 700 ]]
    ' _ "$expected" 'literal argument with spaces'
  )
}

check_runner "$fixture/safe" "$fixture/safe"
check_runner "$fixture/alias" "$fixture/safe"
check_runner "$fixture/unsafe/child" "$(realpath -e /tmp)"

status=0
NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" bash -c 'exit 23' || status=$?
[[ "$status" == 23 ]]
contents=$(find "$fixture/safe" -mindepth 1 -print -quit)
[[ -z "$contents" ]]

status=0
bash "$runner" || status=$?
[[ "$status" == 2 ]]

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
  local fallback="${3:-$candidate}"

  (
    umask 000
    XDG_STATE_HOME="$fixture/ambient-state" NIX_BUILD_TOP="$candidate" TMPDIR="$fallback" bash "$runner" bash -euc '
      [[ "$(umask)" == 0077 ]]
      [[ "$(stat --format=%a -- "$TMPDIR")" == 700 ]]
      [[ "$XDG_STATE_HOME" == "$TMPDIR/state" ]]
      [[ "$(stat --format=%a -- "$XDG_STATE_HOME")" == 700 ]]
      touch "$XDG_STATE_HOME/fixture-lock"
      [[ "$(dirname -- "$TMPDIR")" == "$1" ]]
      [[ "$2" == "literal argument with spaces" ]]
      mkdir "$TMPDIR/child"
      [[ "$(stat --format=%a -- "$TMPDIR/child")" == 700 ]]
    ' _ "$expected" 'literal argument with spaces'
  )
  [[ ! -e "$fixture/ambient-state" ]]
}

check_runner "$fixture/safe" "$fixture/safe"
check_runner "$fixture/alias" "$fixture/safe"
check_runner "$fixture/unsafe/child" "$fixture/safe" "$fixture/safe"

status=0
NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" bash -c 'exit 23' || status=$?
[[ "$status" == 23 ]]
contents=$(find "$fixture/safe" -mindepth 1 -print -quit)
[[ -z "$contents" ]]

status=0
bash "$runner" || status=$?
[[ "$status" == 2 ]]

mkdir "$fixture/bin"
real_stat=$(command -v stat)
bash_path=$(command -v bash)
namespace_owner=65534
if [[ "$EUID" == "$namespace_owner" ]]; then
  namespace_owner=65533
fi
printf '#!%s\n' "$bash_path" > "$fixture/bin/stat"
cat >> "$fixture/bin/stat" <<'SH'
set -euo pipefail
for arg in "$@"; do
  if [[ "$arg" == / ]]; then
    touch "$FIXTURE_ROOT_STAT_CALLED"
    printf '%s 755\n' "$FIXTURE_NAMESPACE_OWNER"
    exit 0
  fi
  if [[ "$arg" == /tmp && "${FIXTURE_UNTRUSTED_TMP:-0}" == 1 ]]; then
    printf '%s 1777\n' "$FIXTURE_NAMESPACE_OWNER"
    exit 0
  fi
done
exec "$FIXTURE_REAL_STAT" "$@"
SH
chmod u+x "$fixture/bin/stat"

(
  export PATH="$fixture/bin:$PATH"
  export FIXTURE_REAL_STAT="$real_stat"
  export FIXTURE_NAMESPACE_OWNER="$namespace_owner"
  export FIXTURE_ROOT_STAT_CALLED="$fixture/root-stat-called"

  check_runner "$fixture/safe" "$fixture/safe"
  check_runner "$fixture/unsafe/child" "$fixture/safe" "$fixture/safe"
  [[ ! -e "$FIXTURE_ROOT_STAT_CALLED" ]]

  status=0
  FIXTURE_UNTRUSTED_TMP=1 NIX_BUILD_TOP="$fixture/unsafe/child" TMPDIR="$fixture/unsafe/child" bash "$runner" touch "$fixture/child-ran" || status=$?
  [[ "$status" == 1 && ! -e "$fixture/child-ran" ]]

  printf '#!%s\nset -euo pipefail\nexit 73\n' "$bash_path" > "$fixture/bin/mktemp"
  chmod u+x "$fixture/bin/mktemp"
  status=0
  NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" touch "$fixture/child-ran" || status=$?
  [[ "$status" == 73 && ! -e "$fixture/child-ran" ]]
)

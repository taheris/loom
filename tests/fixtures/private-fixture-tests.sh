#!/usr/bin/env bash
set -euo pipefail

runner=$(realpath -e -- "$1")
LOOM_TEST_SCHEDULING=$(realpath -e -- "${2:-$(dirname -- "$runner")/test-scheduling.sh}")
export LOOM_TEST_SCHEDULING
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
    CARGO_TARGET_DIR="$fixture/ambient-target" CARGO_BUILD_TARGET_DIR="$fixture/ambient-target" CARGO_BUILD_BUILD_DIR="$fixture/ambient-target" \
      SCCACHE_DIR="$fixture/ambient-cache" SCCACHE_SERVER_UDS="$fixture/ambient.sock" \
      RUSTC_WRAPPER="$fixture/ambient-wrapper" RUSTC_WORKSPACE_WRAPPER="$fixture/ambient-wrapper" \
      CARGO_BUILD_RUSTC_WRAPPER="$fixture/ambient-wrapper" CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER="$fixture/ambient-wrapper" \
      HOME="$fixture/ambient-home" XDG_STATE_HOME="$fixture/ambient-state" NIX_BUILD_TOP="$candidate" TMPDIR="$fallback" bash "$runner" bash -euc '
      [[ "$(umask)" == 0077 ]]
      [[ "$CARGO_TARGET_DIR" == "$TMPDIR/cargo-target" ]]
      [[ "$CARGO_BUILD_TARGET_DIR" == "$CARGO_TARGET_DIR" && "$CARGO_BUILD_BUILD_DIR" == "$CARGO_TARGET_DIR" ]]
      [[ "$SCCACHE_DIR" == "$TMPDIR/sccache" && "$SCCACHE_SERVER_UDS" == "$TMPDIR/sccache.sock" ]]
      [[ "$RUSTC_WRAPPER" == "" && "$RUSTC_WORKSPACE_WRAPPER" == "" ]]
      [[ "$CARGO_BUILD_RUSTC_WRAPPER" == "" && "$CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER" == "" ]]
      [[ "$(stat --format=%a -- "$TMPDIR")" == 700 ]]
      [[ "$HOME" == "$TMPDIR/home" && -d "$HOME" ]]
      [[ "$(stat --format=%a -- "$HOME")" == 700 ]]
      [[ "$XDG_STATE_HOME" == "$TMPDIR/state" ]]
      [[ "$(stat --format=%a -- "$XDG_STATE_HOME")" == 700 ]]
      policy="$XDG_STATE_HOME/loom/trusted-cargo/policy.json"
      [[ "$(stat --format=%a -- "$policy")" == 600 ]]
      [[ "$(stat --format=%h -- "$policy")" == 1 ]]
      [[ "$(<"$policy")" == "{\"reservation_bytes\":1048576}" ]]
      touch "$XDG_STATE_HOME/fixture-lock"
      [[ "$(dirname -- "$TMPDIR")" == "$1" ]]
      [[ "$2" == "literal argument with spaces" ]]
      mkdir "$TMPDIR/child"
      [[ "$(stat --format=%a -- "$TMPDIR/child")" == 700 ]]
    ' _ "$expected" 'literal argument with spaces'
  )
  [[ ! -e "$fixture/ambient-home" ]]
  [[ ! -e "$fixture/ambient-state" && ! -e "$fixture/ambient-target" && ! -e "$fixture/ambient-cache" && ! -e "$fixture/ambient.sock" ]]
}

check_runner "$fixture/safe" "$fixture/safe"
check_runner "$fixture/alias" "$fixture/safe"
check_runner "$fixture/unsafe/child" "$fixture/safe" "$fixture/safe"

# Go fixtures are bounded without clobbering an explicit operator budget.
for workers in "" 3; do
  GOMAXPROCS="$workers" NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" --test-process bash -euc \
    '[[ "$GOMAXPROCS" == "$1" ]]' _ "${workers:-1}"
done

# Runtime tools must not participate in compilation or shadow compiler tools.
mkdir "$fixture/compiler-bin" "$fixture/runtime-bin"
printf '#!%s\nprintf "compiler\\n"\n' "$BASH" > "$fixture/compiler-bin/loom-private-compiler-control"
printf '#!%s\nprintf "shadow\\n"\n' "$BASH" > "$fixture/runtime-bin/loom-private-compiler-control"
printf '#!%s\nprintf "runtime\\n"\n' "$BASH" > "$fixture/runtime-bin/loom-private-runtime-control"
chmod u+x "$fixture/compiler-bin/loom-private-compiler-control" "$fixture/runtime-bin/loom-private-compiler-control" "$fixture/runtime-bin/loom-private-runtime-control"
for mode in --test-process --build-campaign; do
  PATH="$fixture/compiler-bin:$PATH" LOOM_TEST_RUNTIME_PATH="$fixture/runtime-bin" \
    NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" "$mode" bash -euc '
      [[ "$(loom-private-compiler-control)" == compiler ]]
      if [[ "$1" == --test-process ]]; then
        [[ "$(loom-private-runtime-control)" == runtime ]]
      else
        ! command -v loom-private-runtime-control
      fi
    ' _ "$mode"
done

# Store registration uses the declared tool without adding runtime tools to compilation.
mkdir -p "$fixture/registration-tools" "$fixture/registration-git" "$fixture/registration-workspace"
printf '#!%s\n' "$BASH" > "$fixture/registration-tools/nix"
cat >> "$fixture/registration-tools/nix" <<'SH'
set -euo pipefail
[[ "$*" == '--extra-experimental-features nix-command config show sandbox' ]]
printf 'true\n'
SH
printf '#!%s\n' "$BASH" > "$fixture/registration-tools/nix-store"
cat >> "$fixture/registration-tools/nix-store" <<'SH'
set -euo pipefail
[[ "$1" == --load-db ]]
read -r value
[[ "$value" == declared-registration ]]
printf registered > "$REGISTRATION_MARKER"
SH
printf '#!%s\nexit 0\n' "$BASH" > "$fixture/registration-git/git"
chmod u+x "$fixture/registration-tools/nix" "$fixture/registration-tools/nix-store" "$fixture/registration-git/git"
printf 'declared-registration\n' > "$fixture/registration"
(
  cd "$fixture/registration-workspace"
  PATH="$fixture/registration-git:$PATH" \
    LOOM_NATIVE_NIX_BIN="$fixture/registration-tools/nix" \
    LOOM_TEST_NIX_REGISTRATION="$fixture/registration" \
    REGISTRATION_MARKER="$fixture/registered" \
    NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" --build-campaign true
)
[[ "$(<"$fixture/registered")" == registered ]]

# Cleanup handles readonly copies without following links to outside directories.
mkdir -m 500 "$fixture/outside"
NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" --test-process bash -euc '
  mkdir -p "$TMPDIR/mutable/readonly/nested"
  touch "$TMPDIR/mutable/readonly/nested/input"
  chmod -R u-w "$TMPDIR/mutable/readonly"
  ln -s "$1" "$TMPDIR/outside"
' _ "$fixture/outside"
[[ "$(stat --format=%a -- "$fixture/outside")" == 500 ]]
chmod u+w "$fixture/outside"

# Successful runs remove only their own private root.
contents=$(find "$fixture/safe" -mindepth 1 -print -quit)
[[ -z "$contents" ]]

CARGO_TARGET_DIR="$fixture/admitted-target" CARGO_BUILD_TARGET_DIR="$fixture/admitted-target" CARGO_BUILD_BUILD_DIR="$fixture/admitted-target" \
  NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" --build-campaign bash -euc '
    [[ "$CARGO_TARGET_DIR" == "$1" ]]
    [[ "$CARGO_BUILD_TARGET_DIR" == "$1" && "$CARGO_BUILD_BUILD_DIR" == "$1" ]]
    [[ "$XDG_STATE_HOME" == "$TMPDIR/state" ]]
  ' _ "$fixture/admitted-target"
[[ ! -e "$fixture/admitted-target" ]]
contents=$(find "$fixture/safe" -mindepth 1 -print -quit)
[[ -z "$contents" ]]

# Failed runs preserve their original status and diagnostic files.
status=0
NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" bash -c '
  printf "%s\n" "$TMPDIR" > "$1"
  printf "failure evidence\n" > "$TMPDIR/diagnostic"
  exit 23
' _ "$fixture/failed-root" || status=$?
[[ "$status" == 23 ]]
retained=$(<"$fixture/failed-root")
[[ "$(dirname -- "$retained")" == "$fixture/safe" ]]
[[ -d "$retained" && "$(stat --format=%a -- "$retained")" == 700 ]]
[[ $(<"$retained/diagnostic") == 'failure evidence' ]]

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
if [[ "${FIXTURE_TRUNCATED_STAT:-0}" == 1 && "$#" -gt 3 ]]; then
  printf '%s 700\n' "$EUID"
  exit 0
fi
for arg in "$@"; do
  if [[ "$arg" == / ]]; then
    touch "$FIXTURE_ROOT_STAT_CALLED"
    printf '%s 755\n' "$FIXTURE_NAMESPACE_OWNER"
    exit 0
  fi
  if [[ "$arg" == "$FIXTURE_SYSTEM_TMP" && "${FIXTURE_UNTRUSTED_TMP:-0}" == 1 ]]; then
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
  FIXTURE_SYSTEM_TMP=$(realpath -e -- /tmp)
  export FIXTURE_SYSTEM_TMP
  export FIXTURE_NAMESPACE_OWNER="$namespace_owner"
  export FIXTURE_ROOT_STAT_CALLED="$fixture/root-stat-called"

  check_runner "$fixture/safe" "$fixture/safe"
  check_runner "$fixture/unsafe/child" "$fixture/safe" "$fixture/safe"
  [[ ! -e "$FIXTURE_ROOT_STAT_CALLED" ]]

  status=0
  FIXTURE_UNTRUSTED_TMP=1 NIX_BUILD_TOP="$fixture/unsafe/child" TMPDIR="$fixture/unsafe/child" bash "$runner" touch "$fixture/child-ran" || status=$?
  [[ "$status" == 1 && ! -e "$fixture/child-ran" ]]

  status=0
  FIXTURE_TRUNCATED_STAT=1 FIXTURE_UNTRUSTED_TMP=1 NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" touch "$fixture/child-ran" || status=$?
  [[ "$status" == 1 && ! -e "$fixture/child-ran" ]]

  printf '#!%s\nset -euo pipefail\nexit 73\n' "$bash_path" > "$fixture/bin/mktemp"
  chmod u+x "$fixture/bin/mktemp"
  status=0
  NIX_BUILD_TOP="$fixture/safe" TMPDIR="$fixture/safe" bash "$runner" touch "$fixture/child-ran" || status=$?
  [[ "$status" == 73 && ! -e "$fixture/child-ran" ]]
)
# Later successful or rejected invocations cannot erase an earlier failure.
[[ -d "$retained" && $(<"$retained/diagnostic") == 'failure evidence' ]]

#!/usr/bin/env bash
set -euo pipefail

context=$(realpath -e -- "$1")
scheduler=$(realpath -e -- "${2:-$(dirname -- "$context")/test-scheduling.sh}")
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
mkdir -p "$fixture/toolchain/bin" "$fixture/cache" "$fixture/one" "$fixture/two"
for tool in cargo rustc rustdoc; do
  printf '#!%s\nset -euo pipefail\nprintf "fixture tool version\\n"\n' "$(command -v bash)" > "$fixture/toolchain/bin/$tool"
  chmod u+x "$fixture/toolchain/bin/$tool"
done
for workspace in one two; do
  git -C "$fixture/$workspace" init -q
  git -C "$fixture/$workspace" config user.email fixture@example.invalid
  git -C "$fixture/$workspace" config user.name Fixture
done

run_context() {
  local workspace="$1" cache="$2"
  (
    cd "$workspace"
    # Expand context variables only in the child that sources the initializer.
    # shellcheck disable=SC2016
    env LOOM_TEST_TOOLCHAIN="$fixture/toolchain" LOOM_TEST_SCHEDULING="$scheduler" XDG_CACHE_HOME="$cache" \
      PATH="$fixture/toolchain/bin:$PATH" \
      bash -c 'set -euo pipefail; source "$1"; printf "%s\n" "$CARGO_TARGET_DIR"' _ "$context"
  )
}

first=$(run_context "$fixture/one" "$fixture/cache")
printf 'changed source\n' > "$fixture/one/source.rs"
second=$(run_context "$fixture/one" "$fixture/cache")
other=$(run_context "$fixture/two" "$fixture/cache")
[[ "$first" == "$second" && "$first" != "$other" ]]
[[ -d "$first" && ! -L "$first" ]]
if [[ "$(uname -s)" == Linux ]]; then
  [[ "$first" == "$fixture/cache/loom/test-targets/"* ]]
  [[ "$(stat --format=%a -- "$first")" == 700 ]]
  [[ ! -e "$fixture/one/target" && ! -e "$fixture/two/target" ]]
  ln -s "$fixture/cache" "$fixture/alias"
  status=0
  run_context "$fixture/one" "$fixture/alias" > "$fixture/alias.log" 2>&1 || status=$?
  [[ "$status" == 78 ]]
  grep -F 'Test cache is indirect:' "$fixture/alias.log"
  mkdir -m 777 "$fixture/unsafe"
  status=0
  run_context "$fixture/one" "$fixture/unsafe" > "$fixture/unsafe.log" 2>&1 || status=$?
  [[ "$status" == 78 && ! -e "$fixture/unsafe/loom" ]]
  grep -F 'Test cache ancestor is unsafe:' "$fixture/unsafe.log"
fi
# A foreign Cargo cannot silently share the same compiler context.
mkdir "$fixture/foreign"
printf '#!%s\nexit 0\n' "$(command -v bash)" > "$fixture/foreign/cargo"
chmod u+x "$fixture/foreign/cargo"
status=0
(
  cd "$fixture/one"
  # The sourced script and its arguments are evaluated by the child Bash.
  # shellcheck disable=SC2016
  env LOOM_TEST_TOOLCHAIN="$fixture/toolchain" LOOM_TEST_SCHEDULING="$scheduler" PATH="$fixture/foreign:$fixture/toolchain/bin:$PATH" \
    bash -c 'source "$1"' _ "$context"
) > "$fixture/foreign.log" 2>&1 || status=$?
[[ "$status" == 78 ]]
grep -F 'foreign Cargo executable' "$fixture/foreign.log"

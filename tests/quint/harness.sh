#!/usr/bin/env bash
set -euo pipefail

write_script() {
  # Generated executables need an interpreter available inside the Nix sandbox.
  {
    printf '#!%s\n' "$BASH"
    cat
  } >"$1"
  chmod +x "$1"
}

must_fail() {
  local pattern="$1"
  shift
  local status=0
  "$@" >"$tmp/failure" 2>&1 || status=$?
  if [[ "$status" -eq 0 ]] || ! grep -Fq "$pattern" "$tmp/failure"; then
    cat "$tmp/failure" >&2
    printf 'expected visible failure: %s (exit %s)\n' "$pattern" "$status" >&2
    exit 1
  fi
}

test_provisioning_and_batching() {
  local real_quint
  real_quint=$(command -v quint)
  mkdir -p "$tmp/bin"
  export LOOM_REAL_QUINT="$real_quint"
  export LOOM_QUINT_CALLS="$tmp/calls"
  write_script "$tmp/bin/quint" <<'SH'
set -euo pipefail
printf '%s\n' "$1" >> "$LOOM_QUINT_CALLS"
exec "$LOOM_REAL_QUINT" "$@"
SH
  PATH="$tmp/bin:$PATH" "$BASH" "$LOOM_QUINT_SOURCE/scripts/test-quint.sh" provisioning-and-batching >"$tmp/receipts"
  [[ "$(grep -c '^run$' "$tmp/calls")" -eq 3 ]]
  [[ "$(grep -c '^--version$' "$tmp/calls")" -eq 1 ]]
  jq -se 'length == 2 and .[0].plan.seeds == [42,20250308] and .[0].batches[0].progress.states == 168 and .[1].batches[0].progress.states == 2' "$tmp/receipts" >/dev/null
}

test_fail_closed() {
  local script="$LOOM_QUINT_SOURCE/scripts/test-quint.sh"
  must_fail 'unknown scenario' "$BASH" "$script" unknown
  must_fail 'not implemented' "$BASH" "$script" model
  must_fail 'missing provisioned tool' env PATH="$tmp/empty" "$BASH" "$script" provisioning-and-batching
  write_script "$tmp/failed-runner" <<'SH'
set -euo pipefail
if [[ "$1" == budget ]]; then echo 1; else exit 1; fi
SH
  must_fail 'no bounded success' env LOOM_QUINT_BIN="$tmp/failed-runner" "$BASH" "$script" provisioning-and-batching
  write_script "$tmp/failed-runner" <<'SH'
set -euo pipefail
if [[ "$1" == budget ]]; then echo 1; else exit 0; fi
SH
  must_fail 'missing or incomplete bounded receipt' env LOOM_QUINT_BIN="$tmp/failed-runner" "$BASH" "$script" provisioning-and-batching
  write_script "$tmp/failed-runner" <<'SH'
set -euo pipefail
if [[ "$1" == budget ]]; then echo 1; else sleep 5; fi
SH
  must_fail 'exit 124' env LOOM_QUINT_BIN="$tmp/failed-runner" "$BASH" "$script" provisioning-and-batching
  mkdir -p "$tmp/source"
  cp -r "$LOOM_QUINT_SOURCE/crates" "$LOOM_QUINT_SOURCE/tests" "$LOOM_QUINT_SOURCE/nix" "$LOOM_QUINT_SOURCE/scripts" "$tmp/source/"
  cp "$LOOM_QUINT_SOURCE/Cargo.lock" "$LOOM_QUINT_SOURCE/flake.lock" "$LOOM_QUINT_SOURCE/flake.nix" "$tmp/source/"
  chmod -R u+w "$tmp/source"
  printf 'this is not a Quint model\n' >"$tmp/source/tests/fixtures/quint/bridge.qnt"
  must_fail 'Quint returned non-zero' "$LOOM_QUINT_BIN" bridge-replay "$tmp/source"
  cp "$LOOM_QUINT_SOURCE/tests/fixtures/quint/bridge.qnt" "$tmp/source/tests/fixtures/quint/bridge.qnt"
  python3 - "$tmp/source/tests/fixtures/quint/bridge.qnt" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
p.write_text(p.read_text().replace("accepted' = false", "accepted' = true"))
PY
  must_fail 'State invariant failed' "$LOOM_QUINT_BIN" bridge-replay "$tmp/source"
  cp "$LOOM_QUINT_SOURCE/tests/fixtures/quint/bridge.qnt" "$tmp/source/tests/fixtures/quint/bridge.qnt"
  python3 - "$tmp/source/tests/fixtures/quint/bridge.qnt" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
p.write_text(p.read_text().replace("replayObserve", "unsupported"))
PY
  must_fail 'Unimplemented action' "$LOOM_QUINT_BIN" bridge-replay "$tmp/source"
  cp "$LOOM_QUINT_SOURCE/tests/fixtures/quint/bridge.qnt" "$tmp/source/tests/fixtures/quint/bridge.qnt"
  python3 - "$tmp/source/tests/fixtures/quint/bridge.qnt" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
p.write_text(p.read_text().replace("action replayStep = any { replayObserve }", "action replayStep = all { false, count' = count, accepted' = accepted }"))
PY
  must_fail 'incomplete trace batch' "$LOOM_QUINT_BIN" bridge-replay "$tmp/source"
  write_script "$tmp/bin/quint" <<'SH'
set -euo pipefail
exit 0
SH
  must_fail 'zero traces' env PATH="$tmp/bin:$PATH" "$LOOM_QUINT_BIN" bridge-replay "$LOOM_QUINT_SOURCE"
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/empty"
test_provisioning_and_batching
test_fail_closed

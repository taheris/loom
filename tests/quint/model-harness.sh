#!/usr/bin/env bash
set -euo pipefail

: "${LOOM_QUINT_SOURCE:?missing whole-gate model input bundle}"
: "${LOOM_QUINT_PACKAGE:?missing pinned tools}"
root="$LOOM_QUINT_SOURCE"
real_quint=$(command -v quint)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

must_fail() {
  local pattern="$1"
  shift
  local status=0
  "$@" >"$tmp/failure" 2>&1 || status=$?
  if [[ "$status" -eq 0 ]] || ! grep -Fq "$pattern" "$tmp/failure"; then
    cat "$tmp/failure" >&2
    printf 'expected visible model failure: %s (exit %s)\n' "$pattern" "$status" >&2
    exit 1
  fi
}

write_wrapper() {
  {
    printf '#!%s\n' "$BASH"
    cat
  } >"$tmp/bin/quint"
  chmod +x "$tmp/bin/quint"
}

copy_inputs() {
  mkdir -p "$tmp/source/specs" "$tmp/source/scripts" "$tmp/source/tests/quint" "$tmp/source/nix/flake"
  cp -r "$root/specs/gate" "$tmp/source/specs/"
  cp "$root/specs/gate.md" "$root/specs/verify.md" "$root/specs/evidence.md" "$root/specs/simulation.md" "$tmp/source/specs/"
  cp "$root/scripts/test-quint-model.sh" "$root/scripts/test-quint.sh" "$tmp/source/scripts/"
  cp "$root/tests/quint/model-harness.sh" "$tmp/source/tests/quint/"
  cp "$root/nix/flake/quint.nix" "$tmp/source/nix/flake/"
  cp "$root/flake.lock" "$tmp/source/"
  chmod -R u+w "$tmp/source"
}

test_real_model_campaign() {
  "$BASH" "$root/scripts/test-quint.sh" reference-equivalence >"$tmp/receipt.json"
  # Exercise the stdout framing consumed by Gate, not just whole-document JSON.
  [[ "$(wc -l <"$tmp/receipt.json")" -eq 1 ]]
  jq -e '.scenario == "reference-equivalence" and .tests == 81 and
    .completedSeeds == [42,20250308] and .symbolic == null and
    .plan.traces == 64 and .plan.steps == 80 and (.identity | length) == 64' "$tmp/receipt.json" >/dev/null
}

test_invalid_and_empty_campaigns() {
  local script="$root/scripts/test-quint-model.sh"
  must_fail 'unknown model scenario' "$BASH" "$script" unknown
  jq '.routine.traces = 0' "$root/specs/gate/campaign.json" >"$tmp/source/specs/gate/campaign.json"
  must_fail 'invalid or incomplete' env LOOM_QUINT_SOURCE="$tmp/source" "$BASH" "$script" model
  cp "$root/specs/gate/campaign.json" "$tmp/source/specs/gate/campaign.json"
  printf 'not a Quint module\n' >"$tmp/source/specs/gate/model.qnt"
  must_fail 'no bounded success' env LOOM_QUINT_SOURCE="$tmp/source" "$BASH" "$script" witnesses
  cp "$root/specs/gate/model.qnt" "$tmp/source/specs/gate/model.qnt"
  write_wrapper <<'SH'
set -euo pipefail
if [[ "$1" == --version ]]; then printf '0.33.0\n'; fi
SH
  must_fail 'invalid or incomplete' env PATH="$tmp/bin:$PATH" "$BASH" "$script" witnesses
}

test_witness_rejects_always_reject_model() {
  python3 - "$tmp/source/specs/gate/model.qnt" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
p.write_text(p.read_text().replace('with("accepted", authority)', 'with("accepted", false)'))
PY
  must_fail 'no bounded success' env LOOM_QUINT_SOURCE="$tmp/source" "$BASH" "$root/scripts/test-quint-model.sh" witnesses
  cp "$root/specs/gate/model.qnt" "$tmp/source/specs/gate/model.qnt"
}

test_witness_rejects_stale_signer_trust() {
  python3 - "$tmp/source/specs/gate/model.qnt" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
p.write_text(p.read_text().replace('not(f.imported) or s.trust.contains(f.signer),', 'true,', 1))
PY
  must_fail 'no bounded success' env LOOM_QUINT_SOURCE="$tmp/source" "$BASH" "$root/scripts/test-quint-model.sh" cache-model
  cp "$root/specs/gate/model.qnt" "$tmp/source/specs/gate/model.qnt"
}

test_short_trace_cannot_hide_before_full_trace() {
  export LOOM_REAL_QUINT="$real_quint"
  write_wrapper <<'SH'
set -euo pipefail
"$LOOM_REAL_QUINT" "$@"
if [[ "$1" == run ]]; then
  for arg in "$@"; do
    if [[ "$arg" == trace.itf.json ]]; then
      path=trace0.itf.json
      jq '.states = .states[0:1]' "$path" >"$path.short"
      mv "$path.short" "$path"
    fi
  done
fi
SH
  must_fail 'invalid or incomplete' env PATH="$tmp/bin:$PATH" "$BASH" "$root/scripts/test-quint-model.sh" witnesses
}

test_no_symbolic_output_is_not_success() {
  write_wrapper <<'SH'
set -euo pipefail
if [[ "$1" != verify ]]; then exec "$LOOM_REAL_QUINT" "$@"; fi
SH
  must_fail 'missing completed symbolic result' env PATH="$tmp/bin:$PATH" "$BASH" "$root/scripts/test-quint-model.sh" model
}

test_timeout_is_not_completion() {
  {
    printf '#!%s\n' "$BASH"
    printf 'set -euo pipefail\nexit 124\n'
  } >"$tmp/bin/timeout"
  chmod +x "$tmp/bin/timeout"
  must_fail 'exit 124' env PATH="$tmp/bin:$PATH" "$BASH" "$root/scripts/test-quint-model.sh" cache-model
}

mkdir "$tmp/bin"
copy_inputs
test_real_model_campaign
test_invalid_and_empty_campaigns
test_witness_rejects_always_reject_model
test_witness_rejects_stale_signer_trust
test_short_trace_cannot_hide_before_full_trace
test_no_symbolic_output_is_not_success
test_timeout_is_not_completion

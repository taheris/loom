#!/usr/bin/env bash
set -euo pipefail

# Result handling shared by the real campaign and focused negative controls.
# These functions validate outputs; only the campaign executes the model.
require_quint_trace_batch() (
  local directory="$1" traces="$2" states="$3"
  shopt -s nullglob
  local -a batch=("$directory/"*.itf.json)
  if [[ "${#batch[@]}" -ne "$traces" ]]; then
    printf 'test-quint: empty or incomplete whole-gate trace batch; no bounded success\n' >&2
    return 1
  fi
  if ! jq -se --argjson states "$states" 'all(.[]; (.states | length) == $states)' "${batch[@]}" >/dev/null; then
    printf 'test-quint: incomplete whole-gate trace; no bounded success\n' >&2
    return 1
  fi
)

require_quint_exhaustive_result() {
  local output="$1" plan="$2"
  if ! jq -se --slurpfile plan "$plan" '
    length == 1 and (.[0] | .status == "complete" and .backend == "quint-rust-explicit-state-v1" and
    .depth == $plan[0].exhaustive.steps and .roots > 0 and .states > 0 and .transitions > 0)
  ' "$output" >/dev/null; then
    printf 'test-quint: missing completed exhaustive result; no bounded success\n' >&2
    return 1
  fi
}

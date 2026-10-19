#!/usr/bin/env bash
set -euo pipefail

: "${LOOM_QUINT_OWNER_ADMISSION:?missing provisioned admission runner}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

test_original_configuration_fails_without_bounded_success() {
  local status=0
  "$LOOM_QUINT_OWNER_ADMISSION" >"$work/receipt" 2>"$work/diagnostics" || status=$?
  if [[ "$status" -ne 1 ]]; then
    cat "$work/diagnostics" >&2
    printf 'expected original configuration rejection, got exit %s\n' "$status" >&2
    exit 1
  fi
  jq -e '.scenario == 0 and .status == "incomplete" and
    .model == "92c2bd871227acc7df0a0237e47357c30acebac1fadc408b8bb25f1151907d60" and
    .rpc == "fef01065313be0d7cb2b740d61844defae9b81d4" and
    .result == {stage:"verifying",status:"failure",errors:[]}' "$work/receipt" >/dev/null
}

test_admission_rejects_arguments() {
  local status=0
  "$LOOM_QUINT_OWNER_ADMISSION" unexpected >"$work/receipt" 2>"$work/diagnostics" || status=$?
  [[ "$status" -eq 64 && ! -s "$work/receipt" ]]
}

test_original_configuration_fails_without_bounded_success
test_admission_rejects_arguments

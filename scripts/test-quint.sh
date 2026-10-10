#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  printf 'usage: test-quint <scenario>\n' >&2
  exit 64
fi

case "$1" in
  provisioning-and-batching | bridge-smoke)
    campaigns=(routine bridge-replay)
    ;;
  deeper)
    campaigns=(deeper bridge-replay)
    ;;
  model | reference-equivalence | input-generation | cache-model | witnesses | conformance | replay | mutations | incremental | cost-evidence | adoption | workspace-safety-history)
    printf 'test-quint: scenario %s is not implemented; no verification completed\n' "$1" >&2
    exit 1
    ;;
  *)
    printf 'test-quint: unknown scenario %s\n' "$1" >&2
    exit 64
    ;;
esac

: "${LOOM_QUINT_BIN:?missing provisioned Quint acceptance runner}"
: "${LOOM_QUINT_SOURCE:?missing acceptance input bundle}"
: "${LOOM_QUINT_PACKAGE:?missing pinned Quint tool and solver bundle}"

for tool in quint timeout jq; do
  if ! command -v "$tool" >/dev/null; then
    printf 'test-quint: missing provisioned tool %s\n' "$tool" >&2
    exit 1
  fi
done
if [[ "$(quint --version)" != 0.32.0 ]]; then
  printf 'test-quint: incompatible Quint version (required 0.32.0)\n' >&2
  exit 1
fi
if [[ ! -x "$LOOM_QUINT_BIN" || ! -x "$LOOM_QUINT_PACKAGE/share/quint/rust-evaluator-v0.6.0/quint_evaluator" || ! -f "$LOOM_QUINT_PACKAGE/share/quint/apalache-dist-0.56.1/apalache/lib/apalache.jar" ]]; then
  printf 'test-quint: incomplete tool/solver provisioning\n' >&2
  exit 1
fi

output=$(mktemp)
trap 'rm -f "$output"' EXIT
export QUINT_VERBOSE=1
for campaign in "${campaigns[@]}"; do
  seconds=$("$LOOM_QUINT_BIN" budget "$campaign")
  status=0
  timeout --kill-after=5s "${seconds}s" "$LOOM_QUINT_BIN" "$campaign" "$LOOM_QUINT_SOURCE" >"$output" 2>&1 || status=$?
  if [[ "$status" -ne 0 ]]; then
    cat "$output" >&2
    printf 'test-quint: incomplete or failed %s campaign (exit %s); no bounded success\n' "$campaign" "$status" >&2
    exit 1
  fi
  receipt=$(tail -n 1 "$output")
  if ! jq -e '
    . as $r |
    ($r.assurance == "bounded simulation and bridge conformance, not whole-gate proof") and
    ($r.identity | test("^[0-9a-f]{64}$")) and
    ($r.plan.seeds | length > 0) and
    ($r.batches | length) == ($r.plan.seeds | length) and
    all($r.batches[];
      .progress.traces == $r.plan.traces and
      .progress.current_states == ($r.plan.steps + 1) and
      .progress.states == ($r.plan.traces * ($r.plan.steps + 1)))
  ' <<<"$receipt" >/dev/null; then
    cat "$output" >&2
    printf 'test-quint: missing or incomplete bounded receipt\n' >&2
    exit 1
  fi
  printf '%s\n' "$receipt"
done

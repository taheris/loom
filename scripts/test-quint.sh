#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  printf 'usage: test-quint <scenario>\n' >&2
  exit 64
fi

case "$1" in
  todo-acceptance-persistence)
    : "${LOOM_TODO_ACCEPTANCE_BIN:?missing provisioned Todo acceptance runner}"
    exec "$LOOM_TODO_ACCEPTANCE_BIN" acceptance:: --nocapture
    ;;
  remediation-acceptance)
    : "${LOOM_REMEDIATION_ACCEPTANCE_BIN:?missing provisioned remediation acceptance runner}"
    exec "$LOOM_REMEDIATION_ACCEPTANCE_BIN" mint::tests::acceptance:: --nocapture
    ;;
  phase-context)
    : "${LOOM_TASK_ACCEPTANCE_BIN:?missing provisioned context acceptance runner}"
    : "${LOOM_QUINT_SOURCE:?missing context acceptance input bundle}"
    bash "$LOOM_QUINT_SOURCE/scripts/run-private-fixture-tests" "$LOOM_TASK_ACCEPTANCE_BIN" "phase_context_tests::" --nocapture
    bash "$LOOM_QUINT_SOURCE/scripts/run-private-fixture-tests" "$LOOM_TASK_ACCEPTANCE_BIN" "todo::dossier::tests::" --nocapture
    exec bash "$LOOM_QUINT_SOURCE/scripts/run-private-fixture-tests" "$LOOM_TASK_ACCEPTANCE_BIN" "mint::acceptance::dispatch_tests::" --nocapture
    ;;
  task-acceptance-dispatch)
    : "${LOOM_TASK_ACCEPTANCE_BIN:?missing provisioned task acceptance runner}"
    : "${LOOM_QUINT_SOURCE:?missing acceptance input bundle}"
    bash "$LOOM_QUINT_SOURCE/scripts/run-private-fixture-tests" "$LOOM_TASK_ACCEPTANCE_BIN" "r#loop::worker::" --nocapture
    exec bash "$LOOM_QUINT_SOURCE/scripts/run-private-fixture-tests" "$LOOM_TASK_ACCEPTANCE_BIN" "mint::acceptance::dispatch_tests::" --nocapture
    ;;
  workspace-safety-history)
    : "${LOOM_WORKSPACE_SAFETY_BIN:?missing provisioned workspace acceptance runner}"
    if ! command -v podman >/dev/null || ! podman info >/dev/null; then
      printf 'test-quint: skipped workspace-safety-history; container runtime unavailable\n' >&2
      exit 77
    fi
    output=$(mktemp)
    trap 'rm -f "$output"' EXIT
    status=0
    timeout --kill-after=5s 300s "$LOOM_WORKSPACE_SAFETY_BIN" >"$output" 2>&1 || status=$?
    if [[ "$status" -ne 0 ]]; then
      cat "$output" >&2
      printf 'test-quint: failed workspace isolation (exit %s)\n' "$status" >&2
      exit "$status"
    fi
    receipt=$(tail -n 1 "$output")
    if ! jq -e '.scenario == "workspace-safety-history" and .protected_surfaces == 5 and .denied_mutations == 20 and .feedback_runs == 2 and .assurance == "workspace isolation only; safety-fact admission remains Evidence-owned"' <<<"$receipt" >/dev/null; then
      cat "$output" >&2
      printf 'test-quint: missing or incomplete workspace isolation receipt\n' >&2
      exit 1
    fi
    printf '%s\n' "$receipt"
    exit 0
    ;;
  provisioning-and-batching)
    : "${LOOM_QUINT_OWNER_SOURCE:?missing immutable owner inputs}"
    : "${LOOM_QUINT_OWNER_SOLVER:?missing bounded owner solver}"
    for input in specs/gate/{model,domain,reference,witnesses}.qnt specs/gate/{campaign,apalache}.json specs/gate/README.md scripts/quint-{model,symbolic}.sh; do
      if [[ ! -s "$LOOM_QUINT_OWNER_SOURCE/$input" ]]; then
        printf 'test-quint: missing owner input %s\n' "$input" >&2
        exit 1
      fi
    done
    if [[ ! -x "$LOOM_QUINT_OWNER_SOLVER" ]]; then
      printf 'test-quint: bounded owner solver unavailable\n' >&2
      exit 1
    fi
    campaigns=(routine bridge-replay)
    ;;
  bridge-smoke)
    campaigns=(routine bridge-replay)
    ;;
  model | reference-equivalence | cache-model | witnesses)
    : "${LOOM_QUINT_SOURCE:?missing whole-gate model input bundle}"
    exec bash "$LOOM_QUINT_SOURCE/scripts/test-quint-model.sh" "$1"
    ;;
  deeper)
    : "${LOOM_QUINT_SOURCE:?missing whole-gate model input bundle}"
    bash "$LOOM_QUINT_SOURCE/scripts/test-quint-model.sh" deeper
    campaigns=(deeper bridge-replay)
    ;;
  input-generation | conformance | replay | mutations | incremental | cost-evidence | adoption)
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
if [[ "$(quint --version)" != 0.33.0 ]]; then
  printf 'test-quint: incompatible Quint version (required 0.33.0)\n' >&2
  exit 1
fi
if [[ ! -x "$LOOM_QUINT_BIN" || ! -x "$LOOM_QUINT_PACKAGE/share/quint/rust-evaluator-v0.7.0/quint_evaluator" || ! -f "$LOOM_QUINT_PACKAGE/share/quint/apalache-dist-0.62.1/apalache/lib/apalache.jar" ]]; then
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
    ($r.checker == "Quint 0.33.0 / Connect 0.1.2 / Rust evaluator 0.7.0 / Apalache 0.62.1") and
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

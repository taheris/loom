#!/usr/bin/env bash
set -euo pipefail
trap 'printf "test-quint: invalid or incomplete whole-gate verification; no bounded success\n" >&2' ERR

: "${LOOM_QUINT_SOURCE:?missing whole-gate model input bundle}"
: "${LOOM_QUINT_PACKAGE:?missing pinned Quint and Rust checker bundle}"
if [[ "$#" -ne 1 ]]; then
  printf 'usage: test-quint-model <scenario>\n' >&2
  exit 64
fi
case "$1" in
  model | reference-equivalence | cache-model | witnesses) mode=routine ;;
  deeper) mode=deeper ;;
  *) printf 'test-quint: unknown model scenario %s\n' "$1" >&2; exit 64 ;;
esac
for tool in quint loom-quint-checker timeout jq sha256sum; do
  if ! command -v "$tool" >/dev/null; then
    printf 'test-quint: missing provisioned tool %s\n' "$tool" >&2
    exit 1
  fi
done
if [[ "$(quint --version)" != 0.33.0 ]]; then
  printf 'test-quint: incompatible Quint version (required 0.33.0)\n' >&2
  exit 1
fi
if [[ ! -x "$LOOM_QUINT_PACKAGE/share/quint/rust-evaluator-v0.7.0/quint_evaluator" || ! -x "$LOOM_QUINT_PACKAGE/bin/loom-quint-checker" ]]; then
  printf 'test-quint: incomplete Quint/Rust checker provisioning\n' >&2
  exit 1
fi
cd "$LOOM_QUINT_SOURCE"
plan=specs/gate/campaign.json
jq -e '
  def positive: type == "number" and . > 0 and floor == .;
  def campaign:
    (.seeds | length > 0) and (.seeds | unique | length) == (.seeds | length) and
    all(.seeds[]; positive) and all(.traces, .steps, .samples, .seconds; positive);
  .version == 2 and .checker == "Quint 0.33.0 / Rust evaluator 0.7.0 / Rust explicit-state v1" and
  (.routine | campaign) and (.deeper | campaign) and
  .deeper.steps > .routine.steps and .deeper.traces > .routine.traces and
  all(.exhaustive.steps, .exhaustive.seconds, .exhaustive.states, .exhaustive.transitions, .exhaustive.bytes; positive) and (.tests | positive)
' "$plan" >/dev/null
identity=$(sha256sum specs/gate/model.qnt specs/gate/witness.qnt specs/gate/README.md "$plan" \
  specs/gate.md specs/verify.md specs/evidence.md specs/simulation.md \
  scripts/test-quint-model.sh scripts/test-quint.sh tests/quint/model-harness.sh \
  nix/flake/quint.nix nix/quint/tools.nix nix/quint/checker.rs nix/cargo-home.nix flake.lock | sha256sum)
identity="${identity%% *}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

run_checked() {
  local seconds="$1"
  shift
  local status=0
  timeout --kill-after=5s "${seconds}s" "$@" >"$tmp/output" 2>&1 || status=$?
  if [[ "$status" -ne 0 ]]; then
    cat "$tmp/output" >&2
    printf 'test-quint: incomplete or failed whole-gate work (exit %s); no bounded success\n' "$status" >&2
    exit 1
  fi
}

traces=$(jq -r --arg mode "$mode" '.[$mode].traces' "$plan")
steps=$(jq -r --arg mode "$mode" '.[$mode].steps' "$plan")
samples=$(jq -r --arg mode "$mode" '.[$mode].samples' "$plan")
seconds=$(jq -r --arg mode "$mode" '.[$mode].seconds' "$plan")
expected_tests=$(jq -r '.tests' "$plan")
mapfile -t seeds < <(jq -r --arg mode "$mode" '.[$mode].seeds[]' "$plan")
for seed in "${seeds[@]}"; do
  mkdir "$tmp/$seed"
  run_checked "$seconds" quint test specs/gate/witness.qnt --main witness --seed "$seed" \
    --max-samples "$samples" --out "$tmp/$seed/tests.json"
  jq -e --argjson tests "$expected_tests" '
    (.errors | length) == 0 and (.failed | length) == 0 and (.ignored | length) == 0 and
    (.passed | length) == $tests and (.passed | unique | length) == $tests
  ' "$tmp/$seed/tests.json" >/dev/null
  # Quint inserts trace numbers before the first dot, including dots in directories.
  (
    cd "$tmp/$seed"
    run_checked "$seconds" quint run "$LOOM_QUINT_SOURCE/specs/gate/model.qnt" --main gate --seed "$seed" --n-threads 1 \
      --max-samples "$traces" --n-traces "$traces" --max-steps "$steps" --invariant safety \
      --out run.json --out-itf trace.itf.json
  )
  jq -e '.status == "ok" and (.errors | length) == 0' "$tmp/$seed/run.json" >/dev/null
  shopt -s nullglob
  batch=("$tmp/$seed/"*.itf.json)
  if [[ "${#batch[@]}" -ne "$traces" ]]; then
    printf 'test-quint: empty or incomplete whole-gate trace batch; no bounded success\n' >&2
    exit 1
  fi
  jq -se --argjson states "$((steps + 1))" 'all(.[]; (.states | length) == $states)' "${batch[@]}" >/dev/null
  printf 'whole-gate: seed=%s traces=%s states/trace=%s tests=%s complete\n' "$seed" "$traces" "$((steps + 1))" "$expected_tests" >&2
done
printf 'null\n' >"$tmp/exhaustive.json"
if [[ "$1" == model ]]; then
  exhaustive_seconds=$(jq -r '.exhaustive.seconds' "$plan")
  run_checked "$exhaustive_seconds" quint compile specs/gate/model.qnt --main gate --flatten false
  cp "$tmp/output" "$tmp/compiled.json"
  run_checked "$exhaustive_seconds" loom-quint-checker "$tmp/compiled.json" "$plan"
  if ! jq -se --slurpfile plan "$plan" '
    length == 1 and (.[0] | .status == "complete" and .backend == "quint-rust-explicit-state-v1" and
    .depth == $plan[0].exhaustive.steps and .roots > 0 and .states > 0 and .transitions > 0)
  ' "$tmp/output" >/dev/null; then
    printf 'test-quint: missing completed exhaustive result; no bounded success\n' >&2
    exit 1
  fi
  cp "$tmp/output" "$tmp/exhaustive.json"
fi
# Gate reads stdout as JSON lines; keep the diagnostic receipt on one line.
jq -cn --arg scenario "$1" --arg mode "$mode" --arg identity "$identity" --slurpfile exhaustive "$tmp/exhaustive.json" \
  --slurpfile definition "$plan" '{
    assurance: "bounded whole-gate model checking; no implementation conformance or unbounded proof",
    checker: $definition[0].checker, scenario: $scenario, identity: $identity,
    properties: ["referenceEquivalence", "cacheSafety", "publicationSafety"],
    plan: $definition[0][$mode], completedSeeds: $definition[0][$mode].seeds,
    tests: $definition[0].tests, exhaustive: $exhaustive[0]
  }'

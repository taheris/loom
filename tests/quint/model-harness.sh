#!/usr/bin/env bash
set -euo pipefail

: "${LOOM_QUINT_SOURCE:?missing whole-gate model input bundle}"
: "${LOOM_QUINT_PACKAGE:?missing pinned tools}"
root="$LOOM_QUINT_SOURCE"
real_quint=$(command -v quint)
tmp=$(mktemp -d)
cleanup() {
  local status="$?"
  if [[ "$status" -eq 0 ]]; then
    rm -rf "$tmp"
  else
    printf 'model harness failed; retained fixture: %s\n' "$tmp" >&2
  fi
}
trap cleanup EXIT
source "$root/scripts/quint-output.sh"

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
  mkdir -p "$tmp/source/specs" "$tmp/source/scripts" "$tmp/source/tests/quint" "$tmp/source/nix/flake" "$tmp/source/nix/quint"
  cp -r "$root/specs/gate" "$tmp/source/specs/"
  cp "$root/specs/gate.md" "$root/specs/verify.md" "$root/specs/evidence.md" "$root/specs/simulation.md" "$tmp/source/specs/"
  cp "$root/scripts/test-quint-model.sh" "$root/scripts/test-quint.sh" "$root/scripts/quint-output.sh" "$tmp/source/scripts/"
  cp "$root/tests/quint/model-harness.sh" "$root/tests/quint/mutations.py" "$tmp/source/tests/quint/"
  cp "$root/nix/flake/quint.nix" "$tmp/source/nix/flake/"
  cp "$root/nix/quint/tools.nix" "$root/nix/quint/checker.rs" "$root/nix/quint/witnesses.rs" "$tmp/source/nix/quint/"
  cp "$root/nix/cargo-home.nix" "$tmp/source/nix/"
  cp "$root/flake.lock" "$tmp/source/"
  chmod -R u+w "$tmp/source"
}

test_real_model_campaign() {
  # The spy captures real compiler input IR, never a verdict or altered tool output.
  write_wrapper <<'SH'
set -euo pipefail
"$LOOM_REAL_QUINT" "$@"
if [[ "$1" == typecheck ]]; then
  [[ "$#" -eq 4 && "$3" == --out ]]
  cp "$4" "$LOOM_QUINT_CAPTURE"
fi
SH
  env PATH="$tmp/bin:$PATH" LOOM_REAL_QUINT="$real_quint" LOOM_QUINT_CAPTURE="$tmp/witness-ir.json" \
    "$BASH" "$root/scripts/test-quint.sh" reference-equivalence >"$tmp/receipt.json"
  rm "$tmp/bin/quint"
  # Exercise the stdout framing consumed by Gate, not just whole-document JSON.
  [[ "$(wc -l <"$tmp/receipt.json")" -eq 1 ]]
  jq -e '.scenario == "reference-equivalence" and .tests == 113 and
    .completedSeeds == [42,20250308] and .exhaustive == null and
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
  rm "$tmp/bin/quint"
}

test_native_witness_runner_matches_cli() {
  local runner status=0
  runner=$(command -v loom-quint-witnesses)
  "$runner" "$tmp/witness-ir.json" 42 32 >"$tmp/witness-native.json"
  jq -e '(.passed | length) == 113 and (.passed | unique | length) == 113 and
    .failed == [] and .ignored == [] and .errors == []' "$tmp/witness-native.json" >/dev/null
  "$real_quint" test "$root/specs/gate/witness.qnt" --main witness --seed 42 --max-samples 32 \
    --match '^(witnessPublicationTest|referenceAllStagesTest)$' --out "$tmp/witness-cli.json"
  jq -e --slurpfile native "$tmp/witness-native.json" '
    .failed == [] and .ignored == [] and .errors == [] and
    (.passed | sort) == ["referenceAllStagesTest", "witnessPublicationTest"] and
    all(.passed[]; . as $name | ($native[0].passed | index($name)) != null)
  ' "$tmp/witness-cli.json" >/dev/null
  jq '(.table[] | select(.name == "witnessPublicationTest") | .expr) |=
    {kind:"bool", id:.id, value:false}' "$tmp/witness-ir.json" >"$tmp/witness-failure.json"
  "$runner" "$tmp/witness-failure.json" 42 32 >"$tmp/witness-failed.json" 2>"$tmp/witness-failed.log" || status=$?
  [[ "$status" -ne 0 ]]
  jq -e '.failed == ["witnessPublicationTest"] and .ignored == [] and
    (.passed | length) == 112 and [.errors[].code] == ["QNT511"]' "$tmp/witness-failed.json" >/dev/null
  must_fail 'invalid witness sample budget' "$runner" "$tmp/witness-ir.json" 42 0
  jq '(.modules[] | select(.name == "witness") | .declarations) = []' "$tmp/witness-ir.json" >"$tmp/no-witnesses.json"
  must_fail 'empty or oversized witness suite' "$runner" "$tmp/no-witnesses.json" 42 32
  jq '.table = {}' "$tmp/witness-ir.json" >"$tmp/no-lookups.json"
  must_fail 'witness lookup definition not found' "$runner" "$tmp/no-lookups.json" 42 32
}

test_model_mutations() {
  python3 "$root/tests/quint/mutations.py" "$tmp/witness-ir.json" \
    "$LOOM_QUINT_PACKAGE/share/quint/rust-evaluator-v0.7.0/quint_evaluator" "$tmp/failed-mutation-input.json"
}

test_short_trace_cannot_hide_before_full_trace() {
  mkdir "$tmp/traces"
  printf '{"states":[{},{}]}\n' >"$tmp/traces/trace0.itf.json"
  cp "$tmp/traces/trace0.itf.json" "$tmp/traces/trace1.itf.json"
  require_quint_trace_batch "$tmp/traces" 2 2
  jq '.states = .states[0:1]' "$tmp/traces/trace0.itf.json" >"$tmp/short.json"
  mv "$tmp/short.json" "$tmp/traces/trace0.itf.json"
  must_fail 'incomplete whole-gate trace' require_quint_trace_batch "$tmp/traces" 2 2
}

test_empty_trace_batch_is_not_completion() {
  mkdir "$tmp/no-traces"
  must_fail 'empty or incomplete whole-gate trace batch' require_quint_trace_batch "$tmp/no-traces" 2 2
  must_fail 'empty or incomplete whole-gate trace batch' require_quint_trace_batch "$tmp/traces" 3 2
}

test_native_exhaustive_controls() {
  local checker plan status
  checker=$(command -v loom-quint-checker)
  plan="$root/specs/gate/campaign.json"
  "$real_quint" compile "$root/specs/gate/model.qnt" --main gate --flatten false >"$tmp/compiled.json"
  "$checker" "$tmp/compiled.json" "$plan" >"$tmp/exhaustive.json"
  jq -e '.status == "complete" and .roots == 2 and .depth == 1 and .states == 881 and .transitions == 1960' "$tmp/exhaustive.json" >/dev/null
  for field in states bytes transitions; do
    jq --arg field "$field" '.exhaustive[$field] = 1' "$plan" >"$tmp/budget.json"
    must_fail 'budget exhausted' "$checker" "$tmp/compiled.json" "$tmp/budget.json"
  done
  jq '.exhaustive.seconds = 0' "$plan" >"$tmp/budget.json"
  must_fail 'invalid finite exhaustive budget' "$checker" "$tmp/compiled.json" "$tmp/budget.json"
  status=0
  timeout --kill-after=5s 0.001s "$checker" "$tmp/compiled.json" "$plan" >"$tmp/deadline.json" 2>"$tmp/deadline.log" || status=$?
  [[ "$status" -eq 124 ]] && [[ ! -s "$tmp/deadline.json" ]]

  printf '{"main":"gate","main":"gate","modules":[],"table":{}}\n' >"$tmp/malformed.json"
  must_fail 'duplicate field' "$checker" "$tmp/malformed.json" "$plan"
  printf '{' >"$tmp/malformed.json"
  must_fail 'EOF' "$checker" "$tmp/malformed.json" "$plan"
  cat "$tmp/compiled.json" >"$tmp/malformed.json"
  printf 'null\n' >>"$tmp/malformed.json"
  must_fail 'trailing characters' "$checker" "$tmp/malformed.json" "$plan"
  truncate -s 67108865 "$tmp/over-input.json"
  must_fail 'input document budget exhausted' "$checker" "$tmp/over-input.json" "$plan"
  must_fail 'No such file' "$checker" "$tmp/absent.json" "$plan"

  jq '(.modules[] | select(.name == "gate") | .declarations[] | select(.name == "step") | .expr) = {kind:"bool", id:0, value:true}' "$tmp/compiled.json" >"$tmp/uncovered.json"
  must_fail 'unsupported bounded wrapper' "$checker" "$tmp/uncovered.json" "$plan"
  jq '(.modules[] | select(.name == "gate") | .declarations[] | select(.name == "boundedStep") | .expr) = {kind:"app", id:0, opcode:"oneOf", args:[]}' "$tmp/compiled.json" >"$tmp/nondeterministic.json"
  must_fail 'undeclared nondeterminism' "$checker" "$tmp/nondeterministic.json" "$plan"
  jq '(.modules[] | select(.name == "gate") | .declarations) |= map(select(.name != "boundedStep"))' "$tmp/compiled.json" >"$tmp/missing-action.json"
  must_fail 'Input definition not found' "$checker" "$tmp/missing-action.json" "$plan"

  jq '(.modules[] | select(.name == "gate") | .declarations[] | select(.name == "safety") | .expr) |= {kind:"bool", id:.id, value:false}' "$tmp/compiled.json" >"$tmp/counterexample-model.json"
  status=0
  "$checker" "$tmp/counterexample-model.json" "$plan" >"$tmp/counterexample.json" 2>"$tmp/counterexample.log" || status=$?
  [[ "$status" -ne 0 ]]
  jq -e '.status == "violation" and .trace.root == 0 and .trace.choices == [] and .state != null' "$tmp/counterexample.json" >/dev/null
  jq '.trace' "$tmp/counterexample.json" >"$tmp/replay.json"
  status=0
  "$checker" "$tmp/counterexample-model.json" "$plan" "$tmp/replay.json" >"$tmp/replayed.json" 2>"$tmp/replayed.log" || status=$?
  [[ "$status" -ne 0 ]]
  jq -se 'length == 2 and .[0] == .[1]' "$tmp/counterexample.json" "$tmp/replayed.json" >/dev/null
}

test_no_exhaustive_output_is_not_success() {
  local plan="$root/specs/gate/campaign.json"
  require_quint_exhaustive_result "$tmp/exhaustive.json" "$plan"
  printf '' >"$tmp/empty-result.json"
  must_fail 'missing completed exhaustive result' require_quint_exhaustive_result "$tmp/empty-result.json" "$plan"
  jq '.depth += 1' "$tmp/exhaustive.json" >"$tmp/wrong-depth.json"
  must_fail 'missing completed exhaustive result' require_quint_exhaustive_result "$tmp/wrong-depth.json" "$plan"
  jq '.states = 0' "$tmp/exhaustive.json" >"$tmp/no-states.json"
  must_fail 'missing completed exhaustive result' require_quint_exhaustive_result "$tmp/no-states.json" "$plan"
  cat "$tmp/exhaustive.json" "$tmp/exhaustive.json" >"$tmp/two-results.json"
  must_fail 'missing completed exhaustive result' require_quint_exhaustive_result "$tmp/two-results.json" "$plan"
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
test_native_witness_runner_matches_cli
test_model_mutations
test_short_trace_cannot_hide_before_full_trace
test_empty_trace_batch_is_not_completion
test_native_exhaustive_controls
test_no_exhaustive_output_is_not_success
test_timeout_is_not_completion

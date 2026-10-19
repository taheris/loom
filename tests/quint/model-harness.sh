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
  mkdir -p "$tmp/source/specs" "$tmp/source/scripts" "$tmp/source/tests/quint" "$tmp/source/nix/flake" "$tmp/source/nix/quint"
  cp -r "$root/specs/gate" "$tmp/source/specs/"
  cp "$root/specs/gate.md" "$root/specs/verify.md" "$root/specs/evidence.md" "$root/specs/simulation.md" "$tmp/source/specs/"
  cp "$root/scripts/test-quint-model.sh" "$root/scripts/test-quint.sh" "$tmp/source/scripts/"
  cp "$root/tests/quint/model-harness.sh" "$tmp/source/tests/quint/"
  cp "$root/nix/flake/quint.nix" "$tmp/source/nix/flake/"
  cp "$root/nix/quint/tools.nix" "$root/nix/quint/checker.rs" "$tmp/source/nix/quint/"
  cp "$root/nix/cargo-home.nix" "$tmp/source/nix/"
  cp "$root/flake.lock" "$tmp/source/"
  chmod -R u+w "$tmp/source"
}

test_real_model_campaign() {
  "$BASH" "$root/scripts/test-quint.sh" reference-equivalence >"$tmp/receipt.json"
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

assert_model_mutation_fails() {
  local before="$1" after="$2"
  python3 - "$tmp/source/specs/gate/model.qnt" "$before" "$after" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
source = p.read_text()
assert source.count(sys.argv[2]) == 1
p.write_text(source.replace(sys.argv[2], sys.argv[3]))
PY
  must_fail 'no bounded success' env LOOM_QUINT_SOURCE="$tmp/source" "$BASH" "$root/scripts/test-quint-model.sh" witnesses
  cp "$root/specs/gate/model.qnt" "$tmp/source/specs/gate/model.qnt"
}

test_witness_rejects_resource_construction_omission() {
  assert_model_mutation_fails \
    'resources: s.subjects.filter(x => unit(x) == unit(c)).union(Set(8 + unit(c)))' \
    'resources: s.subjects.filter(x => unit(x) == unit(c))'
}

test_witness_rejects_wrong_subject_responsibility() {
  assert_model_mutation_fails 'if (c < 2) 0 else if' 'if (c < 1) 0 else if'
}

test_witness_rejects_passing_readiness_as_coverage() {
  assert_model_mutation_fails \
    'required.forall(c => Set("pass", "pending").contains(coverage(c)))' \
    'required.forall(c => Set("pass", "pending", "UnneededPendingMarker").contains(coverage(c)))'
}

test_witness_rejects_worker_canonical_write_permission() {
  assert_model_mutation_fails \
    'def trustedWriter = Set("operator", "integration").contains(s.checkout)' \
    'def trustedWriter = true'
}

test_witness_rejects_nondurable_checkpoint_deletion() {
  assert_model_mutation_fails \
    's.with("stagedCheckpoint", events).with("checkpointStaged", true)' \
    's.with("checkpoint", events).with("stagedCheckpoint", events).with("checkpointStaged", true)'
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
  write_wrapper <<'SH'
set -euo pipefail
exec "$LOOM_REAL_QUINT" "$@"
SH
  {
    printf '#!%s\n' "$BASH"
    printf 'set -euo pipefail\nexit 0\n'
  } >"$tmp/bin/loom-quint-checker"
  chmod +x "$tmp/bin/loom-quint-checker"
  must_fail 'missing completed exhaustive result' env PATH="$tmp/bin:$PATH" "$BASH" "$root/scripts/test-quint-model.sh" model
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
test_witness_rejects_resource_construction_omission
test_witness_rejects_wrong_subject_responsibility
test_witness_rejects_passing_readiness_as_coverage
test_witness_rejects_worker_canonical_write_permission
test_witness_rejects_nondurable_checkpoint_deletion
test_short_trace_cannot_hide_before_full_trace
test_native_exhaustive_controls
test_no_exhaustive_output_is_not_success
test_timeout_is_not_completion

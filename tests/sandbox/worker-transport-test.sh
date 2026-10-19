#!/usr/bin/env bash
set -euo pipefail

harness="${1:?transport harness is required}"
real_launcher="${2:?real Wrix launcher is required for conformance}"
real_profile="${3:?real Wrix dry-run profile is required}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Wrix dry-run still stages mounts. Never let its cache create Nix's HOME guard.
export HOME="$work/home"
export XDG_CACHE_HOME="$work/cache"
export XDG_CONFIG_HOME="$work/config"
export XDG_DATA_HOME="$work/data"
export XDG_STATE_HOME="$work/state"
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
mkdir -p "$HOME" "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
# Generated scripts are not covered by Nix's patchShebangs phase. In particular,
# /usr/bin/env does not exist in the build sandbox used by this check.
printf '#!%s\n' "$(command -v bash)" > "$work/wrix"
cat >> "$work/wrix" <<'WRIX'
set -euo pipefail
[[ "$#" == 5 && "$1" == --profile-config && "$3" == spawn && "$4" == --spawn-config ]]
profile="$2"
config="$5"
jq -e '.env == [] and .git.deploy == false and .git.sign == false' "$config" >/dev/null
if jq -e '.agent_args == ["--guard-fixture"]' "$config" >/dev/null; then
  jq -e '.mounts | length == 1 and .[0].container_path == "/fixture-guard" and .[0].read_only == true' "$config" >/dev/null
  guard=$(jq -r '.mounts[0].host_path' "$config")
  [[ "$(<"$guard/sentinel")" == 'preserved guard fixture' ]]
  printf 'sandbox-nix-transport-probe-started\n'
  if [[ "$WORKER_TRANSPORT_TEST_MODE" == guard-success ]]; then exit 0; fi
  printf 'worker-profile purity guard is occupied\n' >&2
  exit 1
fi
jq -e '.agent_args == [] and .mounts == []' "$config" >/dev/null
case "$profile" in
  baseline)
    if [[ "$WORKER_TRANSPORT_TEST_MODE" == baseline-failure ]]; then
      printf 'fixture bootstrap failed before probe\n' >&2
      exit 23
    fi
    if [[ "$WORKER_TRANSPORT_TEST_MODE" == empty-success ]]; then exit 0; fi
    printf '%s\n' sandbox-nix-transport-probe-started sandbox-nix-eval-ok sandbox-nix-build-ok \
      'sandbox-nix-trusted-cold-transport-ok iteration=1' \
      'sandbox-nix-trusted-cold-transport-ok iteration=2' sandbox-nix-transport-preflight-ok
    ;;
  transient)
    printf '%s\n' 'fixture transient transfer failure' \
      'sandbox-nix-transport-attempt-failed attempt=1 exit=1' sandbox-nix-transport-preflight-ok
    ;;
  outage)
    if [[ "$WORKER_TRANSPORT_TEST_MODE" == outage-startup-failure ]]; then
      printf 'fixture bootstrap failed before probe\n' >&2
      exit 1
    fi
    printf '%s\n' 'fixture upstream unavailable' 'sandbox-nix-transport-attempt-failed attempt=3 exit=1'
    exit 1
    ;;
  stalled)
    printf '%s\n' 'fixture transfer stalled' 'sandbox-nix-transport-attempt-failed attempt=3 exit=124'
    if [[ "$WORKER_TRANSPORT_TEST_MODE" == stalled-wrong-status ]]; then exit 1; fi
    exit 124
    ;;
  *) exit 97 ;;
esac
WRIX
chmod +x "$work/wrix"
IFS= read -r mock_shebang < "$work/wrix"
[[ "$mock_shebang" == "#!$(command -v bash)" ]]

run_case() {
  local mode="$1"
  local expected="$2"
  local status
  mkdir "$work/$mode"
  if TMPDIR="$work/$mode" WORKER_TRANSPORT_TEST_MODE="$mode" \
    bash "$harness" "$work/wrix" baseline transient outage stalled > "$work/$mode.log" 2>&1; then
    status=0
  else
    status=$?
  fi
  if [[ "$status" != "$expected" ]]; then
    cat "$work/$mode.log" >&2
    printf 'fixture %s: expected exit %s, observed %s\n' "$mode" "$expected" "$status" >&2
    exit 1
  fi
  if [[ "$expected" != 0 ]]; then
    grep -Fq 'retained logs and launch inputs:' "$work/$mode.log"
    [[ -n "$(find "$work/$mode" -name spawn.json -print)" ]]
  else
    grep -Fxq sandbox-nix-transport-failure-paths-ok "$work/$mode.log"
    [[ -z "$(find "$work/$mode" -mindepth 1 -print)" ]]
  fi
}

run_case success 0
run_case baseline-failure 23
run_case empty-success 1
run_case outage-startup-failure 1
run_case stalled-wrong-status 1
run_case guard-success 1
grep -Fq 'fixture bootstrap failed before probe' "$work/baseline-failure.log"
grep -Fq 'baseline exited 23' "$work/baseline-failure.log"
grep -Fq 'baseline missing required evidence:' "$work/empty-success.log"
# Conformance: have the real pinned Wrix parse the exact launch inputs produced
# by the harness, including the safe guard mount, without starting a container.
for mode in baseline-failure guard-success; do
  name=spawn.json
  if [[ "$mode" == guard-success ]]; then name=guard-spawn.json; fi
  config=$(find "$work/$mode" -name "$name" -print)
  [[ -n "$config" ]]
  "$real_launcher" --profile-config "$real_profile" spawn --spawn-config "$config" --dry-run --json \
    > "$work/$mode.dry-run"
  jq -e '.schema == 1 and .plan.agent == "direct" and
    .plan.git_deploy == {enabled:false, origin:"invocation"} and
    .plan.git_sign == {enabled:false, origin:"invocation"} and .plan.credentials == []' \
    "$work/$mode.dry-run" >/dev/null
  if [[ "$mode" == guard-success ]]; then
    jq -e --slurpfile config "$config" '
      .plan.mounts | any(.destination == $config[0].mounts[0].container_path and .source == $config[0].mounts[0].host_path and .read_only == true)
    ' "$work/$mode.dry-run" >/dev/null
  fi
done
printf 'worker-transport-harness-tests-ok\n'

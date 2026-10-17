#!/usr/bin/env bash
set -euo pipefail

launcher="${1:?Wrix launcher is required}"
baseline_profile="${2:?baseline profile is required}"
transient_profile="${3:?transient profile is required}"
outage_profile="${4:?outage profile is required}"
stalled_profile="${5:?stalled profile is required}"
unset WRIX_UNSAFE_PODMAN_SOCKET WRIX_NETWORK WRIX_NETWORK_ALLOWLIST
fixture=$(mktemp -d)
cleanup() {
  local status=$?
  if [[ "$status" == 0 ]]; then
    rm -rf "$fixture"
  else
    printf 'worker-transport: failed; retained logs and launch inputs: %s\n' "$fixture" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
jq -n --arg workspace "$fixture" \
  '{workspace:$workspace,env:[],agent_args:[],mounts:[],git:{deploy:false,sign:false}}' \
  > "$fixture/spawn.json"

launch() {
  local stage="$1"
  local profile="$2"
  local config="$3"
  local status
  printf 'worker-transport: starting %s\n' "$stage"
  # Programmatic spawn deliberately omits --stdio: the direct probe is not an
  # agent RPC session, and must not allocate a TTY or depend on inherited stdin.
  if timeout --kill-after=10s 600s "$launcher" --profile-config "$profile" \
    spawn --spawn-config "$config" < /dev/null > "$fixture/$stage.log" 2>&1; then
    status=0
  else
    status=$?
  fi
  cat "$fixture/$stage.log"
  printf 'worker-transport: %s exited %s\n' "$stage" "$status"
  return "$status"
}
require_line() {
  local stage="$1"
  local canary="$2"
  if ! grep -Fq "$canary" "$fixture/$stage.log"; then
    printf 'worker-transport: %s missing required evidence: %s\n' "$stage" "$canary" >&2
    return 1
  fi
}

launch baseline "$baseline_profile" "$fixture/spawn.json"
require_line baseline sandbox-nix-transport-probe-started
require_line baseline sandbox-nix-eval-ok
require_line baseline sandbox-nix-build-ok
require_line baseline 'sandbox-nix-trusted-cold-transport-ok iteration=1'
require_line baseline 'sandbox-nix-trusted-cold-transport-ok iteration=2'
require_line baseline sandbox-nix-transport-preflight-ok

launch transient "$transient_profile" "$fixture/spawn.json"
require_line transient 'fixture transient transfer failure'
require_line transient 'sandbox-nix-transport-attempt-failed attempt=1 exit=1'
require_line transient sandbox-nix-transport-preflight-ok

if launch outage "$outage_profile" "$fixture/spawn.json"; then
  printf 'transport accepted an unavailable upstream\n' >&2
  exit 1
else
  status=$?
fi
[[ "$status" == 1 ]]
require_line outage 'fixture upstream unavailable'
require_line outage 'sandbox-nix-transport-attempt-failed attempt=3 exit=1'

if launch stalled "$stalled_profile" "$fixture/spawn.json"; then
  printf 'transport accepted an interrupted transfer\n' >&2
  exit 1
else
  status=$?
fi
[[ "$status" == 124 ]]
require_line stalled 'fixture transfer stalled'
require_line stalled 'sandbox-nix-transport-attempt-failed attempt=3 exit=124'

mkdir -m 700 "$fixture/guard"
printf 'preserved guard fixture\n' > "$fixture/guard/sentinel"
# Wrix correctly prohibits mounting onto /homeless-shelter. Mount only an
# ordinary read-only fixture; the trusted disposable probe creates the guard
# alias after Wrix bootstrap, solely to exercise occupied-guard rejection.
jq --arg guard "$fixture/guard" \
  '.agent_args = ["--guard-fixture"] |
   .mounts = [{host_path:$guard,container_path:"/fixture-guard",read_only:true}]' \
  "$fixture/spawn.json" > "$fixture/guard-spawn.json"
if launch guard "$baseline_profile" "$fixture/guard-spawn.json"; then
  printf 'transport accepted an occupied purity guard\n' >&2
  exit 1
else
  status=$?
fi
[[ "$status" == 1 ]]
require_line guard sandbox-nix-transport-probe-started
require_line guard 'worker-profile purity guard is occupied'
[[ "$(cat "$fixture/guard/sentinel")" == 'preserved guard fixture' ]]
printf 'sandbox-nix-transport-failure-paths-ok\n'

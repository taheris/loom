#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 0 ]]; then
  printf 'usage: quint-owner-admission\n' >&2
  exit 64
fi
: "${LOOM_QUINT_OWNER_SOURCE:?missing immutable owner inputs}"
: "${LOOM_APALACHE_BIN:?missing bounded solver}"
: "${LOOM_QUINT_OWNER_IDENTITY:?missing input identity}"
: "${LOOM_QUINT_RPC_IDENTITY:?missing solver identity}"
root="$LOOM_QUINT_OWNER_SOURCE"
definition="$root/specs/gate/campaign.json"
seconds=$(jq -er '.symbolic.seconds | select(type == "number" and . > 0 and . == floor)' "$definition")
heap=$(jq -er '.symbolic.jvm_heap_mb | select(type == "number" and . > 0 and . == floor)' "$definition")
steps=$(jq -er '.symbolic.steps | select(type == "number" and . > 0 and . == floor)' "$definition")
jq -e '.symbolic.cases | index(0) != null' "$definition" >/dev/null
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
status=0
bash "$root/scripts/quint-symbolic.sh" "$seconds" "$heap" \
  "$root/specs/gate/model.qnt" "$root/specs/gate/apalache.json" 0 "$steps" "$work/result.json" >&2 || status=$?
if [[ "$status" -ne 0 ]] || ! jq -e '.stage == "verifying" and .status == "ok" and .errors == []' "$work/result.json" >/dev/null; then
  printf 'quint-owner-admission: incomplete scenario 0; no bounded success\n' >&2
  if [[ -f "$work/result.json" ]]; then
    jq -c --arg model "$LOOM_QUINT_OWNER_IDENTITY" --arg rpc "$LOOM_QUINT_RPC_IDENTITY" \
      '{scenario:0, status:"incomplete", model:$model, rpc:$rpc, result:{stage,status,errors}}' "$work/result.json"
  fi
  exit 1
fi
jq -cn --arg model "$LOOM_QUINT_OWNER_IDENTITY" --arg rpc "$LOOM_QUINT_RPC_IDENTITY" \
  --arg solver "$LOOM_APALACHE_BIN" --slurpfile campaign "$definition" \
  '{scenario:0, status:"ok", model:$model, rpc:$rpc, solver:$solver,
    tools:$campaign[0].tools, symbolic:$campaign[0].symbolic,
    assurance:"input provisioning and bounded scenario 0 only; not full model certification or production conformance"}'

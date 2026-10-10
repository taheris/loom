#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'workspace-safety-history: %s\n' "$1" >&2
  exit 1
}

probe_target() {
  local path="$1"
  [[ ! -e "$path" && ! -r "$path" ]] || fail "host authority visible: $path"
  # Each failing operation is an expected denial against an unmounted host surface.
  if printf 'forged append\n' >>"$path"; then fail "append succeeded: $path"; fi
  if printf 'forged overwrite\n' >"$path"; then fail "overwrite succeeded: $path"; fi
  if rm -- "$path"; then fail "delete succeeded: $path"; fi
  printf 'replacement\n' >/workspace/replacement
  if mv /workspace/replacement "$path"; then fail "replace succeeded: $path"; fi
}

surfaces=0
while IFS= read -r target; do
  probe_target "$target"
  surfaces=$((surfaces + 1))
done < <(jq -r '.[]' /workspace/targets.json)
[[ ! -e /workspace/.loom/integration ]] || fail 'integration checkout exposed'
[[ ! -e /workspace/.loom/marker.json ]] || fail 'integration marker transferred'
mkdir -p /workspace/.loom/safety/witnesses
printf '{"claimed_workspace":"operator","outcome":"pass"}\n' >/workspace/.loom/safety/journal.jsonl
printf 'forged witness\n' >/workspace/.loom/safety/witnesses/object
printf 'forged checkpoint\n' >/workspace/.loom/safety/checkpoint.json
loom gate verify --target 'test -f feedback-ok'
env -u LOOM_INSIDE loom gate verify --target 'test -f feedback-ok'
printf '{"protected_surfaces":%s,"denied_mutations":%s,"feedback_runs":2}\n' "$surfaces" "$((surfaces * 4))" >/workspace/isolation-feedback

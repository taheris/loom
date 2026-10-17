#!/usr/bin/env bash
set -euo pipefail

trap 'printf "sandbox-nix-transport-failed line=%s exit=%s\n" "$LINENO" "$?" >&2' ERR

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

[[ ! -e /workspace/.git && ! -e /workspace/.beads ]]
[[ ! -S /workspace/.wrix/dolt.sock && ! -S /run/wrix/dolt.sock ]]
[[ ! -S /run/podman/podman.sock && ! -S /nix/var/nix/daemon-socket/socket ]]
[[ -z "${WRIX_SIGNING_KEY:-}" && -z "${WRIX_DEPLOY_KEY:-}" ]]
[[ -z "${ANTHROPIC_API_KEY:-}" && -z "${OPENAI_API_KEY:-}" && -z "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]]

if [[ -e /homeless-shelter ]]; then
  printf 'worker-profile purity guard is occupied; preserve it and reprovision the isolated executor after fixing its writer\n' >&2
  exit 1
fi

transport_path=/nix/store/3z3njajk7j5db6j2h80w44almpxqdb83-openssl-3.5.8
transport_hash=sha256-zI9ThXou7IVEj+o8ZOIcKz9zee7EdbLkLeGFgtRzk7k=
upstream=https://cache.nixos.org
nix config show --json > "$work/transport-config.json"
jq -e '."require-sigs".value == true' "$work/transport-config.json" >/dev/null

transfer() {
  local attempt
  local status
  for attempt in 1 2 3; do
    if timeout --kill-after=5s 180s nix "$@"; then
      return 0
    else
      status=$?
    fi
    printf 'sandbox-nix-transport-attempt-failed attempt=%s exit=%s\n' "$attempt" "$status" >&2
    if [[ "$attempt" == 3 ]]; then
      return "$status"
    fi
    sleep 1
  done
}

transfer copy --from "$upstream" "$transport_path"
transfer store copy-sigs --substituter "$upstream" --recursive "$transport_path"
timeout --kill-after=5s 180s nix store verify --recursive "$transport_path"
for iteration in 1 2; do
  cold="$work/cold-$iteration"
  mkdir -m 700 "$cold"
  store="local?root=$cold"
  transfer copy --from "$upstream" --to "$store" "$transport_path"
  timeout --kill-after=5s 180s nix --store "$store" store verify --recursive "$transport_path"
  nix --store "$store" path-info --json "$transport_path" > "$work/transport-$iteration.json"
  jq -e --arg path "$transport_path" --arg hash "$transport_hash" '
    .[$path].narHash == $hash and .[$path].narSize == 9439480
    and (.[$path].signatures | length > 0)
  ' "$work/transport-$iteration.json" >/dev/null
  printf 'sandbox-nix-trusted-cold-transport-ok iteration=%s\n' "$iteration"
done
[[ ! -e /homeless-shelter ]]
printf 'sandbox-nix-transport-preflight-ok\n'

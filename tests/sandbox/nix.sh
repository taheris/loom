#!/usr/bin/env bash
set -euo pipefail

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"

[[ ! -e .git ]]
[[ -z "${NIX_REMOTE:-}" && -z "${NIX_DAEMON_SOCKET_PATH:-}" ]]
[[ ! -S /nix/var/nix/daemon-socket/socket ]]
for ancestor in /nix /nix/var /nix/var/nix; do
  actual=$(stat -c '%u:%a' "$ancestor")
  if [[ "$actual" != 0:755 ]]; then
    printf 'worker Nix-state preflight rejected %s: expected 0:755, observed %s; preserve state and reprovision a conforming isolated profile\n' "$ancestor" "$actual" >&2
    exit 1
  fi
done
nix config show --json | jq -e '."build-users-group".value == "" and .sandbox.value == false' >/dev/null
nix store ping --json | jq -e '.url == "local"' >/dev/null
[[ "$(nix eval --offline --raw --expr '"sandbox-nix-eval-ok"')" == sandbox-nix-eval-ok ]]
printf 'sandbox-nix-eval-ok\n'

system=$(nix eval --offline --impure --raw --expr builtins.currentSystem)
builder=$(realpath "$(command -v bash)")
cat > default.nix <<NIX
builtins.derivation {
  name = "loom-worker-nix-${work##*/}";
  system = "$system";
  builder = "$builder";
  args = [ "-euc" ''printf 'sandbox-nix-build-ok\\n' > "\$out"'' ];
}
NIX

result=$(nix build --offline --no-link --print-out-paths --file ./default.nix)
[[ "$(<"$result")" == sandbox-nix-build-ok ]]
printf 'sandbox-nix-build-ok\n'

# Test the permitted upstream route inside the actual worker, not an operator
# store. Independent empty stores make this a repeatable cold transport probe.
# This establishes dependency availability, never verification/publication proof.
if [[ -e /homeless-shelter ]]; then
  printf 'worker-profile purity guard is occupied; preserve it and reprovision the isolated executor after fixing its writer\n' >&2
  exit 1
fi

transport_path=/nix/store/3z3njajk7j5db6j2h80w44almpxqdb83-openssl-3.5.8
transport_hash=sha256-zI9ThXou7IVEj+o8ZOIcKz9zee7EdbLkLeGFgtRzk7k=
upstream=https://cache.nixos.org
nix config show --json > "$work/transport-config.json"
jq -e '."require-sigs".value == true' "$work/transport-config.json" >/dev/null

# Every operation has an upper deadline. A failed/partial/unsigned transfer or
# timeout is failure, not a skip or a request for a human to run the command.
timeout --kill-after=5s 180s nix copy --from "$upstream" "$transport_path"
timeout --kill-after=5s 180s nix store verify --recursive "$transport_path"
for iteration in 1 2; do
  cold="$work/cold-$iteration"
  mkdir -m 700 "$cold"
  store="local?root=$cold"
  timeout --kill-after=5s 180s nix copy --from "$upstream" --to "$store" "$transport_path"
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

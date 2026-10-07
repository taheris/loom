#!/usr/bin/env bash
set -euo pipefail

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"

[[ ! -e .git ]]
[[ -z "${NIX_REMOTE:-}" && -z "${NIX_DAEMON_SOCKET_PATH:-}" ]]
[[ ! -S /nix/var/nix/daemon-socket/socket ]]
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

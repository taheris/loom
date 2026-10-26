#!/usr/bin/env bash
set -euo pipefail

nix_bin="${1:?patched Nix executable is required}"
bash_package="${2:?immutable Bash package is required}"
coreutils_package="${3:?immutable coreutils package is required}"
registration="${4:?declared input closure registration is required}"

if [[ "${5:-}" != inside ]]; then
  python3 - "$0" "$nix_bin" "$bash_package" "$coreutils_package" "$registration" <<'PY'
import os
import sys

if os.environ.get("NIX_TEST_NONEMPTY_GROUPS") == "1":
    os.setgroups([os.getgid()])
elif os.getgroups():
    os.setgroups([])
os.execvp("unshare", ["unshare", "--user", "--map-root-user", "--mount", "--pid", "--fork", "bash", *sys.argv[1:], "inside"])
PY
  exit 0
fi

mount --make-rprivate /
mount -t proc -o subset=pid,nosuid,nodev,noexec proc /proc
[[ -f /proc/self/status && ! -e /proc/sys && ! -e /proc/cpuinfo ]]
work=$(mktemp -d)
cleanup() {
  local status=$?
  if [[ "$status" == 0 ]]; then
    find -P "$work" -type d -exec chmod u+w {} +
    rm -rf "$work"
  else
    printf 'PID-only procfs fixture retained: %s\n' "$work" >&2
  fi
}
trap cleanup EXIT
export HOME="$work/home"
export NIX_REMOTE=local
export NIX_STATE_DIR="$work/source-state"
export NIX_LOG_DIR="$work/log"
export NIX_CONF_DIR="$work/config"
mkdir -p "$HOME" "$NIX_CONF_DIR"
export NIX_CONFIG='experimental-features = nix-command flakes
build-users-group =
require-sigs = true
sandbox = true
sandbox-fallback = false
require-drop-supplementary-groups = true
sandbox-paths =
extra-sandbox-paths =
substituters =
builders =
allow-import-from-derivation = false'
"$(dirname "$nix_bin")/nix-store" --load-db < "$registration"
store="local?root=$work/store"
nix_store="$(dirname "$nix_bin")/nix-store"
"$nix_store" --query --requisites "$bash_package" "$coreutils_package" > "$work/inputs"
mapfile -t inputs < "$work/inputs"
mkdir -p "$work/store/nix/store"
for input in "${inputs[@]}"; do
  cp -a --no-preserve=ownership --reflink=auto "$input" "$work/store/nix/store/"
done
"$nix_store" --dump-db "${inputs[@]}" > "$work/registration"
"$nix_store" --store "$store" --load-db < "$work/registration"
printf 'must remain outside the sandbox\n' > "$work/undeclared"
parent_pid=$("$coreutils_package/bin/readlink" /proc/self/ns/pid)
parent_net=$("$coreutils_package/bin/readlink" /proc/self/ns/net)
parent_uts=$("$coreutils_package/bin/readlink" /proc/self/ns/uts)
cat > "$work/check.nix" <<NIX
builtins.derivation {
  name = "pid-only-procfs-isolation";
  system = builtins.currentSystem;
  inputs = map builtins.storePath [ "$bash_package" "$coreutils_package" ];
  builder = "$bash_package/bin/bash";
  args = [ "-euc" ''
    test -f /proc/self/status
    while IFS=: read -r key value; do
      if test "\$key" = Groups; then [[ "\$value" =~ ^[[:space:]]*\$ ]]; fi
    done < /proc/self/status
    test ! -e /proc/sys
    test ! -e /proc/cpuinfo
    test ! -e "$work/undeclared"
    test ! -e "$work/source-state"
    test ! -e /proc/1/root$work/undeclared
    test "\$(echo /proc/[0-9]*)" != /proc/\*
    test "\$("$coreutils_package/bin/readlink" /proc/self/ns/pid)" != "$parent_pid"
    test "\$("$coreutils_package/bin/readlink" /proc/self/ns/net)" != "$parent_net"
    test "\$("$coreutils_package/bin/readlink" /proc/self/ns/uts)" != "$parent_uts"
    test "\$("$coreutils_package/bin/uname" -n)" = localhost
    printf 'isolated\\n' > "\$out"
  '' ];
}
NIX
"$nix_bin" build --offline --store "$store" --file "$work/check.nix" --no-link --print-out-paths
"$nix_bin" build --offline --store "$store" --file "$work/check.nix" --no-link --rebuild
printf 'PID-only procfs cold and warm sandbox builds passed\n'

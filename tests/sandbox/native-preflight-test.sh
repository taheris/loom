#!/usr/bin/env bash
set -euo pipefail

preflight_script="${1:?preflight script is required}"
bash_bin=$(command -v bash)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"

if PATH="$work/bin" "$bash_bin" -euo pipefail -s "$preflight_script" > "$work/missing.log" 2>&1 <<'PROBE'; then
source "$1"
require_tool cargo
PROBE
  printf 'native preflight accepted an absent executable\n' >&2
  exit 1
else
  status=$?
fi
[[ "$status" == 78 ]]
grep -F 'capability unavailable: cargo:' "$work/missing.log"

cat > "$work/bin/stat" <<EOF
#!$bash_bin
set -euo pipefail
[[ "\$#" == 3 && "\$1" == -c && "\$2" == '%u:%a' ]]
printf '0:777\\n'
EOF
chmod +x "$work/bin/stat"
if PATH="$work/bin" "$bash_bin" -euo pipefail -s "$preflight_script" > "$work/unsafe.log" 2>&1 <<'PROBE'; then
source "$1"
require_nix_state /fixture/nix
PROBE
  printf 'native preflight accepted unsafe executor ancestry\n' >&2
  exit 1
else
  status=$?
fi
[[ "$status" == 78 ]]
grep -F 'expected 0:755, observed 0:777; preserve state' "$work/unsafe.log"
printf 'native-preflight-failure-contracts-ok\n'

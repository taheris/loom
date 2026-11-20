#!/usr/bin/env bash
set -euo pipefail

[[ "$#" -ge 3 && "${NIX_STATE_DIR:-}" == /* && "$2" == /* ]] || exit 64
case "${NIX_REMOTE:-}" in
    local|local://\?real=*) ;;
    *) exit 64 ;;
esac
nix="$1"
physical_store="$2"
shift 2
for registration in "$@"; do
    "$nix"-store --load-db < "$registration"
done
mkdir -p "$physical_store" "$NIX_STATE_DIR/gcroots"
"$nix" --extra-experimental-features nix-command path-info --all --offline > "$NIX_STATE_DIR/fixture-inputs"
while IFS= read -r input; do
    if [[ ! -e "$input" && ! -L "$input" ]]; then
        printf 'Registered fixture input is physically absent: %s\n' "$input" >&2
        exit 1
    fi
    if [[ "$physical_store" != "${NIX_STORE_DIR:-/nix/store}" && ! -e "$physical_store/${input##*/}" && ! -L "$physical_store/${input##*/}" ]]; then
        cp -a --no-preserve=ownership --reflink=auto -- "$input" "$physical_store/${input##*/}"
    fi
    ln -s -- "$input" "$NIX_STATE_DIR/gcroots/${input##*/}"
done < "$NIX_STATE_DIR/fixture-inputs"
"$nix"-store --verify --check-contents

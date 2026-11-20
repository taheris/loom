#!/usr/bin/env bash
set -euo pipefail

[[ "$#" == 1 ]] || exit 64
loader=$(realpath -e -- "$1")
fixture=$(mktemp -d)
fixture=$(realpath -e -- "$fixture")
cleanup() {
    local status=$?
    if [[ "$status" == 0 ]]; then
        rm -rf -- "$fixture"
    else
        printf 'Fixture input evidence retained at %s\n' "$fixture" >&2
    fi
}
trap cleanup EXIT
export NIX_USER_CONF_FILES=/dev/null
export NIX_LOG_DIR="$fixture/log" NIX_CONF_DIR="$fixture/conf" HOME="$fixture/home"
export NIX_CONFIG=$'experimental-features = nix-command flakes\nbuild-users-group =\nsubstituters =\nbuilders ='
mkdir -p "$HOME" "$NIX_CONF_DIR"

for protection in unprotected retained copied; do
    export NIX_REMOTE=local
    export NIX_STORE_DIR="$fixture/$protection/store" NIX_STATE_DIR="$fixture/$protection/primary-state"
    mkdir -p "$fixture/$protection"
    printf 'immutable fixture input\n' > "$fixture/$protection/die-hook"
    input=$(nix-store --add "$fixture/$protection/die-hook")
    nix-store --dump-db > "$fixture/$protection/registration"
    [[ -e "$input" ]]
    export NIX_STATE_DIR="$fixture/$protection/fixture-state"
    physical_store="$NIX_STORE_DIR"
    if [[ "$protection" == copied ]]; then
        physical_store="$fixture/$protection/physical"
        export NIX_REMOTE="local://?real=$physical_store"
    fi
    physical="$physical_store/${input##*/}"
    if [[ "$protection" != unprotected ]]; then
        bash "$loader" "$(command -v nix)" "$physical_store" "$fixture/$protection/registration"
        printf 'disposable private output\n' > "$fixture/$protection/private-output"
        output=$(nix-store --add "$fixture/$protection/private-output")
    else
        nix-store --load-db < "$fixture/$protection/registration"
    fi
    nix-store --debug --gc > "$fixture/$protection/gc.stdout" 2> "$fixture/$protection/gc.stderr"
    cat "$fixture/$protection/gc.stdout" "$fixture/$protection/gc.stderr"
    if [[ "$protection" != unprotected ]]; then
        [[ -e "$physical" && ! -e "$physical_store/${output##*/}" ]]
        if nix-store --delete "$input"; then
            printf 'Registered fixture input was not protected from deletion\n' >&2
            exit 1
        fi
        rm -rf -- "$NIX_STATE_DIR"
        [[ -e "$physical" ]]
    else
        [[ ! -e "$input" ]]
    fi
    export NIX_REMOTE=local NIX_STATE_DIR="$fixture/$protection/primary-state"
    nix-store --check-validity "$input"
    nix-store --realise "$input"
    if [[ "$protection" == unprotected ]]; then
        [[ ! -e "$input" ]]
    else
        [[ -e "$input" ]]
        [[ "$(< "$input")" == 'immutable fixture input' ]]
    fi
done

export NIX_REMOTE=local NIX_STATE_DIR="$fixture/absent-state" NIX_STORE_DIR="$fixture/unprotected/store"
if bash "$loader" "$(command -v nix)" "$NIX_STORE_DIR" "$fixture/unprotected/registration" > "$fixture/absent.log" 2>&1; then
    printf 'Absent registered inputs were accepted\n' >&2
    exit 1
fi
grep -F 'Registered fixture input is physically absent:' "$fixture/absent.log"
printf 'Reproduced split-state deletion; rooted and copied fixture inputs survive GC, deletion and cleanup while private outputs remain collectible.\n'

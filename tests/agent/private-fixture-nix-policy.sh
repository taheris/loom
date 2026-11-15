#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" != 2 ]]; then
    printf 'Usage: private-fixture-nix-policy.sh <native-wrapper> <packaged-wrapper>\n' >&2
    exit 2
fi

root=$(mktemp -d)
trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root/home" "$root/registry"
: > "$root/registration"
export NIX_BIN
NIX_BIN=$(command -v nix)
export NEXTEST_TEST_THREADS=1
unset LOOM_TEST_NIX_REGISTRATION
export NIX_CONFIG=$'experimental-features = nix-command flakes\nsubstitute = true\ncores = 2'

for wrapper in "$@"; do
    wrapper=$(realpath -e -- "$wrapper")
    for mode in --test-process --build-campaign; do
        expected=true
        if [[ "$mode" == --test-process ]]; then expected=false; fi
        env -u CARGO_HOME HOME="$root/home" \
            EXPECTED_CARGO_HOME="$root/home/.cargo" EXPECTED_SUBSTITUTE="$expected" \
            ORIGINAL_HOME="$root/home" \
            bash "$wrapper" "$mode" bash <<'CHILD'
set -euo pipefail
test "$CARGO_HOME" = "$EXPECTED_CARGO_HOME"
test "$HOME" != "$ORIGINAL_HOME"
test "$("$NIX_BIN" config show substitute)" = "$EXPECTED_SUBSTITUTE"
test "$("$NIX_BIN" config show cores)" = 2
CHILD
    done
    CARGO_HOME="$root/registry" HOME="$root/home" EXPECTED_CARGO_HOME="$root/registry" \
        bash "$wrapper" --test-process bash <<'CHILD'
set -euo pipefail
test "$CARGO_HOME" = "$EXPECTED_CARGO_HOME"
CHILD
    env -u CARGO_HOME HOME=/homeless-shelter \
        bash "$wrapper" --test-process bash <<'CHILD'
set -euo pipefail
test "${CARGO_HOME:-$HOME/.cargo}" != /homeless-shelter/.cargo
CHILD
    for sandbox in true false; do
        workspace="$root/policy-${wrapper##*/}-$sandbox"
        mkdir -p "$workspace"
        printf 'isolated policy fixture\n' > "$workspace/README"
        (
            cd "$workspace"
            NIX_CONFIG="${NIX_CONFIG}"$'\nsandbox = '"$sandbox" \
                EXPECTED_SANDBOX="$sandbox" \
                LOOM_TEST_NIX_REGISTRATION="$root/registration" \
                LOOM_NATIVE_NIX_BIN="$NIX_BIN" \
                bash "$wrapper" --build-campaign bash <<'CHILD'
set -euo pipefail
test "$("$NIX_BIN" config show sandbox)" = "$EXPECTED_SANDBOX"
test "$("$NIX_BIN" config show sandbox-fallback)" = false
test "$("$NIX_BIN" config show require-sigs)" = true
test "$("$NIX_BIN" config show allow-import-from-derivation)" = false
test -z "$("$NIX_BIN" config show substituters)"
test -z "$("$NIX_BIN" config show builders)"
test "$("$NIX_BIN" config show cores)" = 2
CHILD
        )
    done
done

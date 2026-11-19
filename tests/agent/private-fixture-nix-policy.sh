#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" != 1 ]]; then
    printf 'Usage: private-fixture-nix-policy.sh <wrapper>\n' >&2
    exit 2
fi

root=$(mktemp -d)
trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root/home" "$root/registry"
: > "$root/registration"
# Exercise the runner's actual private physical store, without a tool adapter.
export NIX_BIN
NIX_BIN=$(command -v nix)
export NEXTEST_TEST_THREADS=1
unset LOOM_TEST_NIX_REGISTRATION
export NIX_CONFIG=$'experimental-features = nix-command flakes\nsubstitute = true\ncores = 2\nrequire-sigs = false\nstore = daemon'

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
    for registration in "$root/registration" "${LOOM_POLICY_REGISTRATION:-$root/registration}"; do
    for sandbox in true false; do
        workspace=$(mktemp -d "$root/policy.XXXXXXXX")
        expected_seed=
        if [[ -s "$registration" ]]; then expected_seed="${LOOM_POLICY_SEED:?missing declared seed}"; fi
        printf 'isolated policy fixture\n' > "$workspace/README"
        (
            cd "$workspace"
            NIX_CONFIG="${NIX_CONFIG}"$'\nsandbox = '"$sandbox" \
                EXPECTED_SANDBOX="$sandbox" \
                EXPECTED_SEED="$expected_seed" LOOM_TEST_NIX_REGISTRATION="$registration" \
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
# Adapters discard ambient selectors, but must retain the private store policy.
unset NIX_CONFIG NIX_REMOTE
test "$("$NIX_BIN" config show store)" = "local://?real=$TMPDIR/store"
test "$("$NIX_BIN" config show sandbox)" = "$EXPECTED_SANDBOX"
test "$("$NIX_BIN" config show sandbox-fallback)" = false
"$NIX_BIN" store ping --json > /dev/null
test -d "$TMPDIR/store/.links"
test -f "$NIX_STATE_DIR/db/db.sqlite"
test ! -e .git
if [[ -n "$EXPECTED_SEED" ]]; then
    physical="$TMPDIR/store/${EXPECTED_SEED##*/}"
    cmp "$EXPECTED_SEED" "$physical"
    chmod u+w "$physical"
    printf 'corrupted SDK bytes\n' > "$physical"
    if "$NIX_BIN"-store --verify --check-contents; then
        printf 'Private SDK content corruption was accepted\n' >&2
        exit 1
    fi
    test "$(< "$EXPECTED_SEED")" = 'declared SDK bytes'
fi
CHILD
        )
    done
    done
done

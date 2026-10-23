#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd) # best-effort: allow invocation outside a checkout.
cd "$repo_root"

export GIT_CONFIG_GLOBAL="$repo_root/tests/fixtures/git/test-gitconfig"
export GIT_CONFIG_SYSTEM=/dev/null
unset WRIX_SIGNING_KEY

if [[ -f "$repo_root/scripts/configure-cargo-patches.sh" ]]; then
    bash "$repo_root/scripts/configure-cargo-patches.sh" "${LOOM_CARGO_PATCHES:-}"
fi

python3 tests/verify/native-resources.py "$repo_root"
cargo build --workspace
mkdir -p "$repo_root/.loom/logs"
nextest_run_dir=$(mktemp -d "$repo_root/.loom/logs/required-suite.XXXXXXXX")
nextest_log="$nextest_run_dir/nextest.log"
printf 'Nextest transcript: %s\n' "$nextest_log" >&2
export NEXTEST_FAILURE_OUTPUT=immediate-final
export NEXTEST_STATUS_LEVEL=fail
export NEXTEST_FINAL_STATUS_LEVEL=fail
export NEXTEST_SHOW_PROGRESS=none
if cargo nextest run --workspace --ignore-default-filter 2>&1 | tee "$nextest_log"; then
    :
else
    nextest_status=("${PIPESTATUS[@]}")
    printf 'Nextest failed; transcript: %s (runner=%s, recorder=%s)\n' \
        "$nextest_log" "${nextest_status[0]}" "${nextest_status[1]}" >&2
    if [[ "${nextest_status[0]}" != 0 ]]; then
        exit "${nextest_status[0]}"
    fi
    exit 78
fi
nix run .#test-native-units

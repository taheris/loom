#!/usr/bin/env bash
set -euo pipefail

prepare_only=false
if [[ "$#" == 1 && "$1" == --prepare-only ]]; then
    prepare_only=true
elif [[ "$#" != 0 ]]; then
    printf 'Usage: test-required [--prepare-only]\n' >&2
    exit 64
fi

repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd) # best-effort: allow invocation outside a checkout.
cd "$repo_root"

export GIT_CONFIG_GLOBAL="$repo_root/tests/fixtures/git/test-gitconfig"
export GIT_CONFIG_SYSTEM=/dev/null
unset WRIX_SIGNING_KEY

if [[ -f "$repo_root/scripts/configure-cargo-patches.sh" ]]; then
    bash "$repo_root/scripts/configure-cargo-patches.sh" "${LOOM_CARGO_PATCHES:-}"
fi

loom-verification native-resources "$repo_root"
unset LOOM_TEST_DECLARED_SOURCES_FILE
export LOOM_NATIVE_RUST_FIXTURE LOOM_NATIVE_SMOKE_FIXTURE
LOOM_NATIVE_RUST_FIXTURE=$(nix build .#native-rust-fixture --offline --no-substitute --no-link --print-out-paths)
LOOM_NATIVE_SMOKE_FIXTURE=$(nix build .#native-smoke-fixture --offline --no-substitute --no-link --print-out-paths)
cargo build --workspace
if [[ "$prepare_only" == true ]]; then
    printf '%s\n' 'Native suite inputs prepared; no tests executed.'
    exit 0
fi
mkdir -p "$repo_root/.loom/logs"
nextest_run_dir=$(mktemp -d "$repo_root/.loom/logs/required-suite.XXXXXXXX")
nextest_log="$nextest_run_dir/nextest.log"
printf 'Nextest transcript: %s\n' "$nextest_log" >&2
export NEXTEST_FAILURE_OUTPUT=immediate-final
export NEXTEST_STATUS_LEVEL=fail
export NEXTEST_FINAL_STATUS_LEVEL=fail
export NEXTEST_SHOW_PROGRESS=none
if [[ -z "${NEXTEST_TEST_THREADS:-}" ]]; then
    NEXTEST_TEST_THREADS=$(bash "$repo_root/nix/test-scheduling.sh")
    export NEXTEST_TEST_THREADS
fi
printf 'Native test scheduling: %s threads\n' "$NEXTEST_TEST_THREADS" >&2
nextest=(cargo nextest run)
if [[ -n "${LOOM_TEST_COMPILATION?the app must select its compiler owner}" ]]; then
    printf 'Immutable test compiler: %s\n' "$LOOM_TEST_COMPILATION" >&2
    export LOOM_TEST_SOURCE_ROOT="$repo_root" INSTA_WORKSPACE_ROOT="$repo_root"
    nextest+=(
        --ignore-default-filter --no-fail-fast
        --cargo-metadata "$LOOM_TEST_COMPILATION/cargo.json"
        --binaries-metadata "$LOOM_TEST_COMPILATION/binaries.json"
        --workspace-remap "$repo_root"
        --target-dir-remap "$LOOM_TEST_COMPILATION/target"
    )
else
    nextest+=(--workspace --ignore-default-filter)
fi
if "${nextest[@]}" 2>&1 | tee "$nextest_log"; then
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
# Kernel/container conformance is not an ordinary Loom suite. The pushed-range
# Gate plan retains separately admitted affected edge obligations.

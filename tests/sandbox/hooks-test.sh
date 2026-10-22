#!/usr/bin/env bash
set -euo pipefail

source_root=$(realpath "${1:?usage: hooks-test.sh <source-root> <cargo-patches>}")
patch_config=$(realpath "${2:?fixed Cargo patch configuration is required}")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/source/bin" "$work/source/scripts" "$work/source/tests/sandbox" "$work/source/.cargo"
cp "$source_root/Cargo.lock" "$work/source/"
cp "$source_root/.cargo/config.toml" "$work/source/.cargo/"
cp "$source_root/bin/pre-push-checks" "$work/source/bin/"
cp "$source_root/scripts/check-shell-reexec" "$source_root/scripts/configure-cargo-patches.sh" \
    "$source_root/scripts/cargo-patch-key.nix" "$work/source/scripts/"
cp "$source_root/tests/sandbox/verify-payload.sh" "$work/source/tests/sandbox/"

assert_rejects_policy_mutation() {
    local mutation="$1"
    local diagnostic="$2"
    sed "$mutation" "$source_root/.pre-commit-config.yaml" > "$work/source/.pre-commit-config.yaml"
    if bash "$source_root/tests/sandbox/hooks.sh" "$work/source" "$patch_config" > "$work/output" 2>&1; then
        printf 'sandbox verifier accepted policy mutation: %s\n' "$mutation" >&2
        exit 1
    fi
    if ! grep -Fq "$diagnostic" "$work/output"; then
        printf 'unexpected failure for %s:\n%s\n' "$mutation" "$(<"$work/output")" >&2
        exit 1
    fi
}

test_real_hook_policy_passes() {
    bash "$source_root/tests/sandbox/hooks.sh" "$source_root" "$patch_config"
}

test_disabled_file_gate_is_detected() {
    assert_rejects_policy_mutation 's/entry: loom gate verify --files/entry: true/' \
        'gate did not check payload: good'
}

test_disabled_native_initialization_is_detected() {
    assert_rejects_policy_mutation 's/entry: bash scripts\/configure-cargo-patches.sh/entry: true/' \
        'native Cargo hook did not initialize patches'
}

test_disabled_formatter_is_detected() {
    assert_rejects_policy_mutation 's/entry: treefmt --fail-on-change/entry: true/' \
        'ordinary commit bypassed treefmt'
}

test_disabled_pre_push_gate_is_detected() {
    assert_rejects_policy_mutation 's/--append-push-range -- loom gate verify --diff/--append-push-range -- true/' \
        'gate did not check payload: updated'
}

test_disabled_nix_check_is_detected() {
    assert_rejects_policy_mutation 's/-- skip-if-missing nix -- nix flake check$/-- true/' \
        'Nix flake check did not build fixture'
}

test_disabled_nix_required_app_is_detected() {
    assert_rejects_policy_mutation 's/-- skip-if-missing nix -- nix run .#test-required$/-- true/' \
        'Nix required-test app did not run'
}

test_real_hook_policy_passes
test_disabled_file_gate_is_detected
test_disabled_native_initialization_is_detected
test_disabled_formatter_is_detected
test_disabled_pre_push_gate_is_detected
test_disabled_nix_check_is_detected
test_disabled_nix_required_app_is_detected
printf 'sandbox-hook-self-tests-ok\n'

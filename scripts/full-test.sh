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

nix fmt -- --fail-on-change
nix build
nix flake check --no-warn-dirty
cargo build --workspace
cargo clippy --workspace -- -D warnings
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
loom gate system --tree

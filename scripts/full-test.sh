#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd) # best-effort: allow invocation outside a checkout.
cd "$repo_root"

export GIT_CONFIG_GLOBAL="$repo_root/tests/fixtures/git/test-gitconfig"
export GIT_CONFIG_SYSTEM=/dev/null
unset WRIX_SIGNING_KEY

nix flake check --no-warn-dirty
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
loom gate system --tree

#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd) # best-effort: Nix checks run in git-less source artifacts.
config="${1:-}"
if [[ -z "$config" ]]; then
    mkdir -p "$repo_root/.loom"
    config=$(nix build --out-link "$repo_root/.loom/cargo-patches" --print-out-paths "$repo_root#cargo-patches")
fi
if [[ ! -f "$config" ]]; then
    printf 'Cargo patch configuration is missing: %s\n' "$config" >&2
    exit 1
fi

mkdir -p "$repo_root/.cargo"
temporary=$(mktemp "$repo_root/.cargo/nix-patches.XXXXXX")
trap 'rm -f "$temporary"' EXIT
ln -sf "$config" "$temporary"
mv -f "$temporary" "$repo_root/.cargo/nix-patches.toml"

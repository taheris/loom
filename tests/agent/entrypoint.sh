#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == --print-inputs || "${2:-}" == --print-inputs ]]; then
    printf '%s\n' '{"inputs":["flake.lock","nix/flake/lib.nix","nix/flake/checks.nix","nix/workspace.nix","nix/patches","tests/agent/entrypoint.sh","tests/agent/entrypoint.py"]}'
    exit 0
fi

selector="${1:---all}"
case "$selector" in
    test_pi_rpc | test_claude_stdio | test_direct_stdio | test_shared_setup)
        tests=("Entrypoint.$selector")
        ;;
    --all)
        tests=()
        ;;
    *)
        printf 'Unknown entrypoint check: %s\n' "$selector" >&2
        exit 1
        ;;
esac

# Nix checks supply the same patched source without nested Nix evaluation.
source_path="${LOOM_AGENT_WRIX_SOURCE:-}"
if [[ -z "$source_path" ]]; then
    source_path=$(nix build --no-link --print-out-paths .#wrixSrc)
fi
script_dir=$(dirname "${BASH_SOURCE[0]}")
exec python3 "$script_dir/entrypoint.py" "$source_path" "${tests[@]}"

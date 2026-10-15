#!/usr/bin/env bash
set -euo pipefail

git_dir=$(git rev-parse --absolute-git-dir)
if [[ -f "$git_dir/smoke-verifier-hold" ]]; then
    printf '%s %s\n' "$$" "$PPID" > "$git_dir/smoke-verifier-entered.pending"
    mv "$git_dir/smoke-verifier-entered.pending" "$git_dir/smoke-verifier-entered"
    for _attempt in $(seq 1 180); do
        if [[ ! -f "$git_dir/smoke-verifier-hold" ]]; then
            break
        fi
        sleep 1
    done
    if [[ -f "$git_dir/smoke-verifier-hold" ]]; then
        printf 'smoke verifier hold expired; required coverage remains pending\n' >&2
        exit 124
    fi
fi
test -f loom-smoke-result.txt

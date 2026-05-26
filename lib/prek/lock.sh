#!/usr/bin/env bash
set -euo pipefail

_prek_pid_alive() {
    # kill -0 emits "No such process" on stderr for absent PIDs;
    # we use exit code as a boolean, so suppress that expected noise.
    kill -0 "$1" 2>/dev/null
}

_prek_acquire_lock() {
    local workspace_basename lock_dir lock_file deadline now holder_pid

    if ! command -v flock >/dev/null 2>&1; then
        echo "prek lock: flock missing on PATH; install util-linux (Linux) or flock (macOS Nix package)" >&2
        return 1
    fi

    workspace_basename="$(basename "$(git worktree list --porcelain | awk '/^worktree / {print $2; exit}')")"
    lock_dir="${XDG_STATE_HOME:-$HOME/.local/state}/loom/prek/${workspace_basename}"
    lock_file="${lock_dir}/prek.lock"

    mkdir -p "$lock_dir"

    exec 9<>"$lock_file"

    deadline=$(( $(date +%s) + 600 ))

    while :; do
        if flock -x -n 9; then
            printf '%s\n' "$$" > "$lock_file"
            return 0
        fi

        holder_pid=""
        if [[ -s "$lock_file" ]]; then
            holder_pid="$(cat "$lock_file")"
        fi

        if [[ -n "$holder_pid" ]] && ! _prek_pid_alive "$holder_pid"; then
            echo "prek lock: reclaiming lock from dead PID ${holder_pid}" >&2
            rm -f "$lock_file"
            exec 9>&-
            exec 9<>"$lock_file"
            continue
        fi

        now=$(date +%s)
        if (( now >= deadline )); then
            echo "prek lock: timeout after 600s; holder PID=${holder_pid:-unknown}" >&2
            return 1
        fi

        echo "prek lock: waiting on holder PID ${holder_pid:-unknown}" >&2
        sleep 1
    done
}

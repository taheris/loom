#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--probe" ]]; then
    wrix="${2:?missing Wrix service launcher}"
    # Each Wrix probe already polls readiness; never restart the service or rerun the smoke.
    for attempt in {1..5}; do
        if "$wrix" service dolt wait; then
            printf '[smoke] Dolt ready (startup probe %s/5)\n' "$attempt" >&2
            exit 0
        fi
        printf '[smoke] Dolt still starting (startup probe %s/5 failed)\n' "$attempt" >&2
    done
    exit 1
fi

wrix="${1:?missing Wrix service launcher}"
if timeout --signal=TERM --kill-after=2s 30s bash "$0" --probe "$wrix"; then
    exit 0
fi

printf '[smoke] error: Dolt readiness failed within the 30-second startup budget\n' >&2
for diagnostic in status logs; do
    printf '[smoke] Wrix service %s:\n' "$diagnostic" >&2
    if ! "$wrix" service "$diagnostic" >&2; then
        printf '[smoke] could not collect Wrix service %s\n' "$diagnostic" >&2
    fi
done
exit 1

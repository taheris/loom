#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--print-inputs" ]]; then
    printf '{"inputs":["payload.txt"]}\n'
    exit 0
fi

payload=$(<payload.txt)
printf '%s\n' "$payload" >> .git/payload-checks
if [[ "$payload" == "good" || "$payload" == "updated" ]]; then
    printf '{"pass":true,"evidence":"sandbox payload accepted"}\n'
else
    printf '{"pass":false,"evidence":"sandbox payload rejected"}\n'
    exit 1
fi

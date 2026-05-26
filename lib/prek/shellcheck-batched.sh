#!/usr/bin/env bash
set -euo pipefail

if [[ $# -eq 0 ]]; then
    exit 0
fi

files=("$@")
total=$#
exit_code=0

for ((i=0; i<total; i+=25)); do
    chunk=("${files[@]:i:25}")
    if ! shellcheck "${chunk[@]}"; then
        exit_code=1
    fi
done

exit "$exit_code"

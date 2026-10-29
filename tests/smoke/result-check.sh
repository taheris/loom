#!/usr/bin/env bash
set -euo pipefail
[[ $# == 2 ]] || exit 64
bash "$2" --hook-id smoke-noop --hook-entry true -- true
IFS= read -r result < "$1"
if [[ "$result" != implemented ]]; then
    printf 'smoke result is not implemented\n' >&2
    exit 1
fi
printf 'verified\n' > "${out:?native output required}"

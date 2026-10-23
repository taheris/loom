#!/usr/bin/env bash
set -euo pipefail
[[ $# == 1 ]] || exit 64
IFS= read -r result < "$1"
if [[ "$result" != implemented ]]; then
    printf 'smoke result is not implemented\n' >&2
    exit 1
fi
printf 'verified\n' > "${out:?native output required}"

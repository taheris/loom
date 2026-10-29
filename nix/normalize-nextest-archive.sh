#!/usr/bin/env bash
set -euo pipefail
[[ $# == 2 ]] || exit 64
input="$1"
output="$2"
work=$(mktemp -d)
export LC_ALL=C
# Nextest records wall-clock mtimes for metadata and freshly linked binaries.
# Preserve payload bytes, paths, links and modes; canonicalize only packaging.
zstd --decompress --stdout "$input" | tar --extract --same-permissions --no-same-owner --directory "$work"
tar --sort=name --mtime=@1 --owner=0 --group=0 --numeric-owner --format=gnu \
    --directory "$work" --create --file - target | zstd --threads=1 --quiet --stdout > "$output"

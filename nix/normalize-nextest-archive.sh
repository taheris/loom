#!/usr/bin/env bash
set -euo pipefail
[[ $# == 2 ]] || exit 64
input="$1"
output="$2"
work=$(mktemp -d)
export LC_ALL=C
# Nextest records wall-clock mtimes for metadata and freshly linked binaries.
# Keep binary payloads intact; canonicalize packaging and relocatable target paths.
zstd --decompress --stdout "$input" | tar --extract --same-permissions --no-same-owner --directory "$work"
metadata="$work/target/nextest/binaries-metadata.json"
root=$(jq -r '.["rust-build-meta"]["target-directory"] // empty' "$metadata")
if [[ -n "$root" ]]; then
    [[ "$root" == /* && "$root" != / ]] || exit 65
    for name in binaries-metadata cargo-metadata; do
        path="$work/target/nextest/$name.json"
        jq --sort-keys --arg root "$root" '
          walk(if type == "string" then
            if . == $root then "/target"
            elif startswith($root + "/") then "/target" + ltrimstr($root)
            else . end
          else . end)
        ' "$path" > "$path.normalized"
        mv "$path.normalized" "$path"
    done
fi
tar --sort=name --mtime=@1 --owner=0 --group=0 --numeric-owner --format=gnu \
    --directory "$work" --create --file - target | zstd --threads=1 --quiet --stdout > "$output"

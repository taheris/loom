#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -lt 3 ]]; then
    printf 'usage: run-smoke.sh <fuse-device> <tun-device> <runtime-flake-ref> [args...]\n' >&2
    exit 64
fi

fuse_device="$1"
tun_device="$2"
runtime_flake_ref="$3"
shift 3

if [[ ! -c "$fuse_device" || ! -c "$tun_device" ]]; then
    printf '[smoke] skip: container runtime devices %s and %s are required\n' \
        "$fuse_device" "$tun_device" >&2
    exit 77
fi

runtime=$(nix build --no-link --print-out-paths "$runtime_flake_ref")
if [[ ! -x "$runtime/bin/smoke" ]]; then
    printf '[smoke] error: runtime %s does not provide bin/smoke\n' "$runtime" >&2
    exit 1
fi

exec "$runtime/bin/smoke" "$@"

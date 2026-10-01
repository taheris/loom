#!/usr/bin/env bash
set -euo pipefail

skip() {
    printf 'test-direct-workspace: skipped; %s\n' "$1" >&2
    exit 77
}

if [[ "$(uname -s)" != Linux ]]; then
    skip 'requires Linux container mounts'
fi
if [[ -f /.dockerenv || -f /run/.containerenv ]] && [[ ! -e /dev/fuse ]]; then
    skip 'nested container execution requires /dev/fuse'
fi
if ! command -v podman >/dev/null 2>&1; then
    skip 'podman is not available'
fi

tmpdir=$(mktemp -d)
cleanup() {
    local status=$?
    rm -rf "$tmpdir" || printf 'test-direct-workspace: cleanup failed: %s\n' "$tmpdir" >&2
    exit "$status"
}
trap cleanup EXIT

podman_args=(--root "$tmpdir/storage" --runroot "$tmpdir/runroot")
if ! podman "${podman_args[@]}" info > "$tmpdir/info" 2>&1; then
    if grep -Eiq 'operation not permitted|cannot clone|cannot re-exec|newuidmap|newgidmap' "$tmpdir/info"; then
        skip "$(<"$tmpdir/info")"
    fi
    printf 'test-direct-workspace: podman failed: %s\n' "$(<"$tmpdir/info")" >&2
    exit 1
fi

cargo test -p loom-agent --test workspace_mount --no-run --message-format=json > "$tmpdir/artifacts"
probe=$(jq -rs '[.[] | select(.reason == "compiler-artifact" and .target.name == "workspace_mount" and .executable != null) | .executable] | if length == 1 then .[0] else error("expected one mount-test executable") end' "$tmpdir/artifacts")
image=$(nix build --no-link --print-out-paths .#sandbox-image)
"$image" | podman "${podman_args[@]}" load > "$tmpdir/load" 2>&1
ref=$(awk '/^Loaded image(s)?: / {sub(/^Loaded image(s)?: /, ""); print; exit}' "$tmpdir/load")
if [[ -z "$ref" ]]; then
    printf 'test-direct-workspace: image reference missing: %s\n' "$(<"$tmpdir/load")" >&2
    exit 1
fi

mkdir -p "$tmpdir/workspace/nested"
canary="mount-${tmpdir##*/}"
printf '%s\r\nmounted workspace\n' "$canary" > "$tmpdir/workspace/nested/probe.txt"

# The host-linked test executable needs its immutable dynamic-linker closure.
podman "${podman_args[@]}" run --rm --network=none \
    --volume /nix/store:/nix/store:ro \
    --volume "$probe:/mount-test:ro" \
    --volume "$tmpdir/workspace:/workspace:ro" \
    --env "LOOM_MOUNT_CANARY=$canary" \
    --entrypoint /mount-test "$ref" \
    --ignored --exact direct_tools_read_against_container_workspace_mount | tee "$tmpdir/result"
if ! grep -Fq 'test result: ok. 1 passed; 0 failed; 0 ignored;' "$tmpdir/result"; then
    printf 'test-direct-workspace: expected exactly one executed mount test\n' >&2
    exit 1
fi

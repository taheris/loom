#!/usr/bin/env bash
set -euo pipefail

is_unsupported_container_runtime_error() {
    local text="${1,,}"
    case "$text" in
        *"/dev/fuse"* | *"/dev/net/tun"* | *"fuse-overlayfs"* | *"mount proc"* | *mount*proc*permission*denied* | *"oci permission denied"* | *"cannot clone"* | *"cannot re-exec process"* | *"newuidmap"* | *"newgidmap"* | *"operation not permitted"* | *"netavark"* | *"pasta"* | *"slirp4netns"*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

skip_unsupported_container_runtime() {
    local reason="$1"
    printf 'test-sandbox: skipped; nested container execution is unavailable:\n%s\n' "$reason" >&2
    exit 77
}

running_inside_container() {
    [[ -f /.dockerenv || -f /run/.containerenv ]]
}

require_nested_device() {
    local path="$1"
    local reason="$2"
    if [[ "${LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS:-}" == "1" ]]; then
        return
    fi
    if running_inside_container && [[ ! -e "$path" ]]; then
        skip_unsupported_container_runtime "$reason"
    fi
}

require_nested_device /dev/fuse "running inside a container without /dev/fuse; podman cannot mount the sandbox filesystem."
require_nested_device /dev/net/tun "running inside a container without /dev/net/tun; podman cannot configure sandbox networking."

if ! command -v podman >/dev/null 2>&1; then
    skip_unsupported_container_runtime "podman is not available on PATH."
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT
export HOME="$tmpdir"
mkdir -p "$HOME/.config/containers"
printf '%s\n' '{"default":[{"type":"insecureAcceptAnything"}]}' > "$HOME/.config/containers/policy.json"

podman_args=(--root "$tmpdir/storage" --runroot "$tmpdir/runroot")

if ! info_out=$(podman "${podman_args[@]}" info 2>&1); then
    if is_unsupported_container_runtime_error "$info_out"; then
        skip_unsupported_container_runtime "$info_out"
    fi
    printf 'test-sandbox: podman info failed:\n%s\n' "$info_out" >&2
    exit 1
fi

sandbox_image="${LOOM_SANDBOX_IMAGE:-}"
if [[ -z "$sandbox_image" ]]; then
    if [[ -z "${LOOM_SANDBOX_IMAGE_ATTR:-}" ]]; then
        printf 'test-sandbox: LOOM_SANDBOX_IMAGE or LOOM_SANDBOX_IMAGE_ATTR must be set\n' >&2
        exit 2
    fi
    if ! command -v nix >/dev/null 2>&1; then
        printf 'test-sandbox: nix is not available on PATH\n' >&2
        exit 2
    fi
    build_err="$tmpdir/nix-build.err"
    if ! sandbox_image=$(nix build --no-link --print-out-paths "$LOOM_SANDBOX_IMAGE_ATTR" 2> "$build_err"); then
        printf 'test-sandbox: sandbox image build failed:\n%s\n' "$(cat "$build_err")" >&2
        exit 1
    fi
fi

if ! load_out=$({ "$sandbox_image" | podman "${podman_args[@]}" load; } 2>&1); then
    if is_unsupported_container_runtime_error "$load_out"; then
        skip_unsupported_container_runtime "$load_out"
    fi
    printf 'test-sandbox: podman load failed:\n%s\n' "$load_out" >&2
    exit 1
fi

ref=$(printf '%s\n' "$load_out" | sed -nE 's/^Loaded image: (.+)$/\1/p; s/^Loaded image\(s\): (.+)$/\1/p' | head -n1)
if [[ -z "$ref" ]]; then
    printf 'test-sandbox: could not parse image ref from podman load output:\n%s\n' "$load_out" >&2
    exit 1
fi

if ! run_out=$(podman "${podman_args[@]}" run --rm --network=none --cgroups=disabled --entrypoint=/bin/bash "$ref" -lc 'set -euo pipefail; pi --version >/dev/null' 2>&1); then
    if is_unsupported_container_runtime_error "$run_out"; then
        skip_unsupported_container_runtime "$run_out"
    fi
    printf 'test-sandbox: pi --version failed:\n%s\n' "$run_out" >&2
    exit 1
fi

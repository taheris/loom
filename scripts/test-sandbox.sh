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

if ! command -v podman >/dev/null 2>&1; then
    skip_unsupported_container_runtime "podman is not available on PATH."
fi

cleanup_tmpdir() {
    local status=$?
    if [[ -n "${tmpdir:-}" && -d "$tmpdir" ]]; then
        chmod -R u+rwX "$tmpdir" 2>/dev/null || true # best-effort: cleanup must not mask the test result.
        rm -rf "$tmpdir" 2>/dev/null || true # best-effort: cleanup must not mask the test result.
    fi
    exit "$status"
}

tmpdir=$(mktemp -d)
trap cleanup_tmpdir EXIT
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

workspace_source="${LOOM_TEST_SANDBOX_SOURCE:-$(pwd -P)}"
if [[ ! -f "$workspace_source/Cargo.toml" || ! -d "$workspace_source/.git" ]]; then
    printf 'test-sandbox: run from the Loom repository root\n' >&2
    exit 2
fi

bead_workspace="${LOOM_TEST_SANDBOX_WORKSPACE:-}"
if [[ -z "$bead_workspace" ]]; then
    bead_workspace="$tmpdir/bead-workspace"
    git -c core.hooksPath=/dev/null clone --quiet --local "$workspace_source" "$bead_workspace"
fi

dolt_mount_args=()
if [[ -f "$bead_workspace/.beads/metadata.json" ]]; then
    if ! beads_backend=$(jq -er '.backend // "sqlite"' "$bead_workspace/.beads/metadata.json"); then
        printf 'test-sandbox: could not parse Beads backend metadata\n' >&2
        exit 2
    fi
    if [[ "$beads_backend" == "dolt" ]]; then
        host_dolt_socket="$workspace_source/.wrix/dolt.sock"
        if [[ ! -S "$host_dolt_socket" && -S "${BEADS_DOLT_SERVER_SOCKET:-}" ]]; then
            host_dolt_socket="$BEADS_DOLT_SERVER_SOCKET"
        fi
        if [[ ! -S "$host_dolt_socket" ]]; then
            printf 'test-sandbox: host Dolt socket is unavailable; start the workspace Wrix service\n' >&2
            exit 1
        fi
        mkdir -p "$bead_workspace/.wrix"
        dolt_mount_args=(--volume "$host_dolt_socket:/workspace/.wrix/dolt.sock")
    fi
fi

if ! run_out=$(podman "${podman_args[@]}" run --rm --network=none --env WRIX_AGENT=pi \
    --cap-add=NET_ADMIN --cgroups=disabled \
    --volume "$bead_workspace:/workspace:rw" "${dolt_mount_args[@]}" \
    "$ref" /bin/bash -lc '
        set -euo pipefail
        pi --version >/dev/null
        cd /workspace
        if command -v nix >/dev/null 2>&1; then
            printf "nix unexpectedly present in the worker image\n" >&2
            exit 1
        fi
        if [[ -z "${WRIX_PREK_HOOKS:-}" || ! -x "$WRIX_PREK_HOOKS/pre-commit" ]]; then
            printf "canonical WRIX_PREK_HOOKS installation is unavailable\n" >&2
            exit 1
        fi
        configured_hooks=$(git config --local --get core.hooksPath)
        if [[ "$configured_hooks" != "$WRIX_PREK_HOOKS" ]]; then
            printf "entrypoint configured core.hooksPath=%s, expected %s\n" "$configured_hooks" "$WRIX_PREK_HOOKS" >&2
            exit 1
        fi
        git config user.email test@example.com
        git config user.name "Sandbox Hook Test"
        git config commit.gpgsign false
        printf "agent change\n" > sandbox-hook-test.txt
        git add sandbox-hook-test.txt
        commit_output=$(git commit -m "Exercise sandbox hooks" 2>&1)
        case "$commit_output" in
            *"treefmt --fail-on-change"*"loom gate verify --files"*) ;;
            *)
                printf "agent commit did not traverse the real pre-commit chain:\n%s\n" "$commit_output" >&2
                exit 1
                ;;
        esac
        from_ref=$(git rev-parse HEAD^)
        to_ref=$(git rev-parse HEAD)
        pre_push_output=$(prek run nix-flake-check loom-gate-verify-diff \
            --stage pre-push --from-ref "$from_ref" --to-ref "$to_ref" --verbose 2>&1)
        case "$pre_push_output" in
            *"nix flake check"*"loom gate verify --diff"*) ;;
            *)
                printf "sandbox pre-push did not skip Nix and execute the non-Nix gate hook:\n%s\n" "$pre_push_output" >&2
                exit 1
                ;;
        esac
        printf "sandbox-hook-chain-ok\n"
    ' 2>&1); then
    if is_unsupported_container_runtime_error "$run_out"; then
        skip_unsupported_container_runtime "$run_out"
    fi
    printf 'test-sandbox: sandbox verification failed:\n%s\n' "$run_out" >&2
    exit 1
fi

if [[ "$run_out" != *"sandbox-hook-chain-ok"* ]]; then
    printf 'test-sandbox: sandbox hook-chain canary missing:\n%s\n' "$run_out" >&2
    exit 1
fi

#!/usr/bin/env bash
set -euo pipefail

# Real Loom, prek, Git and Wrix exercise the host gate without a container.
wrix_bin="${1:?absolute Wrix launcher path is required}"
pre_push_checks="${2:?pre-push wrapper path is required}"
native_fixture="${3:?immutable native predicate fixture is required}"
store_registration="${4:?declared native input closure registration is required}"
umask 077
fixture=$(mktemp -d "$(cd "${TMPDIR:-/tmp}" && pwd -P)/loom-host-gate.XXXXXXXX")
cleanup_fixture() {
    local status=$?
    trap - EXIT
    if [[ "$status" -ne 0 ]]; then
        printf 'host gate fixture retained after failure (exit %s): %s\n' "$status" "$fixture" >&2
        exit "$status"
    fi
    # Nix sources are readonly. Only this disposable tree's directories need
    # write permission for unlinking; never chmod files or follow symlinks.
    find -P "$fixture" -type d -exec chmod u+w {} +
    rm -rf "$fixture"
}
trap cleanup_fixture EXIT
export HOME="$fixture/home"
export TMPDIR="$fixture/tmp"
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
# env -i drops host Nix configuration; declare the CLI features this fixture uses.
export NIX_CONFIG="${NIX_CONFIG-}"$'\nextra-experimental-features = nix-command flakes'
export WRIX_DEPLOY_KEY="$fixture/smoke"
export WRIX_SIGNING_KEY="$fixture/smoke-signing"
mkdir -p "$HOME" "$TMPDIR" "$fixture/workspace"
cd "$fixture/workspace"

ssh-keygen -q -t ed25519 -N "" -f "$WRIX_DEPLOY_KEY"
ssh-keygen -q -t ed25519 -N "" -f "$WRIX_SIGNING_KEY"
git init -q --bare "$fixture/origin.git"
git init -q -b main
git remote add origin "$fixture/origin.git"
git config user.name "Smoke Host Gate"
git config user.email smoke@example.com
"$wrix_bin" init repo --offline --no-hooks --sign --key smoke

cp -R "$native_fixture/." .
chmod -R u+w docs specs verification.nix result-check.sh flake.nix loom-smoke-result.txt .gitignore
# This fixture specifically exercises the required system lane before hooks.
spec=$(< specs/smoke.md)
printf '%s\n' "${spec/\[check\]/[system]}" > specs/smoke.md
# These scripts belong to this outer test harness, not to the inner project.
# Keep them outside its inventory while executing the real configured hook.
harness="$fixture/harness"
mkdir -p "$harness" .loom
cp "$pre_push_checks" "$harness/pre-push-checks"
cat > "$harness/host-gate-hook.sh" <<'HOOK'
#!/usr/bin/env bash
set -euo pipefail
printf 'ran\n' > .loom/host-hook-ran
[[ ! -e .loom/reject-hook ]]
loom gate check --tree
HOOK
cat > .pre-commit-config.yaml <<YAML
repos:
  - repo: local
    hooks:
      - id: smoke-host-gate
        name: smoke host gate
        entry: bash "$harness/pre-push-checks" --hook-id smoke-host-gate --hook-entry 'bash "$harness/host-gate-hook.sh"' -- bash "$harness/host-gate-hook.sh"
        language: system
        stages: [pre-commit]
        always_run: true
        pass_filenames: false
YAML
git add .gitignore .pre-commit-config.yaml docs specs flake.nix verification.nix result-check.sh loom-smoke-result.txt
git commit -q -m "Initialize host gate fixture"
printf 'implemented\n' > loom-smoke-result.txt
git add loom-smoke-result.txt
git commit -q -m "Implement smoke bead"
git verify-commit HEAD

# Own the store database inside this fixture. The enclosing build already
# supplies these readonly store objects; register only its declared Bash/Nix
# closure, without a host daemon, substitution, or an implicit HOME store.
export NIX_REMOTE=local
export NIX_STORE_DIR=/nix/store
export NIX_STATE_DIR="$fixture/nix-state"
export NIX_LOG_DIR="$fixture/nix-log"
export NIX_CONF_DIR="$fixture/nix-conf"
mkdir -p "$NIX_STATE_DIR" "$NIX_LOG_DIR" "$NIX_CONF_DIR"
cat > "$NIX_CONF_DIR/nix.conf" <<'NIX'
experimental-features = nix-command flakes
build-users-group =
require-sigs = true
sandbox = true
sandbox-fallback = false
allow-import-from-derivation = false
substituters =
builders =
NIX
nix-store --load-db < "$store_registration"

test_smoke_runtime_executes_host_hook() {
    loom gate verify --diff HEAD^..HEAD
    [[ "$(< .loom/host-hook-ran)" == "ran" ]]
}

test_smoke_runtime_rejects_failing_host_hook() {
    local rc
    rm .loom/host-hook-ran
    touch .loom/reject-hook
    if loom gate verify --diff HEAD^..HEAD > "$fixture/rejected.log" 2>&1; then
        printf 'host gate accepted a failing hook\n' >&2
        exit 1
    else
        rc=$?
    fi
    if [[ "$rc" -ne 1 || ! -s .loom/host-hook-ran ]]; then
        cat "$fixture/rejected.log" >&2
        printf 'expected the executed hook to fail the host gate (exit %s)\n' "$rc" >&2
        exit 1
    fi
}

test_smoke_runtime_rejects_failing_required_native_check() {
    local rc
    rm .loom/reject-hook .loom/host-hook-ran
    printf 'not implemented\n' > loom-smoke-result.txt
    if loom gate verify --diff HEAD^..HEAD > "$fixture/system-rejected.log" 2>&1; then
        printf 'host gate accepted a failing required native check\n' >&2
        exit 1
    else
        rc=$?
    fi
    if [[ "$rc" -ne 1 ]]; then
        cat "$fixture/system-rejected.log" >&2
        printf 'expected executed native check to reject host gate (exit %s)\n' "$rc" >&2
        exit 1
    fi
    grep -F 'loom gate [system] FAIL: nix build .#checks.' "$fixture/system-rejected.log"
    [[ "$(< .loom/host-hook-ran)" == "ran" ]]
    [[ "$(git --git-dir="$fixture/origin.git" rev-list --all --count)" == 0 ]]
    printf 'implemented\n' > loom-smoke-result.txt
    loom gate verify --diff HEAD^..HEAD
    [[ "$(< .loom/host-hook-ran)" == "ran" ]]
}

test_smoke_runtime_executes_host_hook
test_smoke_runtime_rejects_failing_host_hook
test_smoke_runtime_rejects_failing_required_native_check

#!/usr/bin/env bash
set -euo pipefail

# Real Loom, prek, Git and Wrix exercise the host gate without a container.
wrix_bin="${1:?absolute Wrix launcher path is required}"
pre_push_checks="${2:?pre-push wrapper path is required}"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
export HOME="$fixture/home"
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
export WRIX_DEPLOY_KEY="$fixture/smoke"
export WRIX_SIGNING_KEY="$fixture/smoke-signing"
mkdir -p "$HOME" "$fixture/workspace"
cd "$fixture/workspace"

ssh-keygen -q -t ed25519 -N "" -f "$WRIX_DEPLOY_KEY"
ssh-keygen -q -t ed25519 -N "" -f "$WRIX_SIGNING_KEY"
git init -q --bare "$fixture/origin.git"
git init -q -b main
git remote add origin "$fixture/origin.git"
git config user.name "Smoke Host Gate"
git config user.email smoke@example.com
"$wrix_bin" init --offline --no-hooks --sign --key smoke

mkdir -p bin specs .loom
cp "$pre_push_checks" bin/pre-push-checks
printf '.loom/\n' > .gitignore
printf '# Smoke\n' > specs/smoke.md
cat > bin/host-gate-hook.sh <<'HOOK'
#!/usr/bin/env bash
set -euo pipefail
printf 'ran\n' > .loom/host-hook-ran
[[ ! -e .loom/reject-hook ]]
HOOK
cat > .pre-commit-config.yaml <<'YAML'
repos:
  - repo: local
    hooks:
      - id: smoke-host-gate
        name: smoke host gate
        entry: bash bin/pre-push-checks --hook-id smoke-host-gate --hook-entry 'bash bin/host-gate-hook.sh' -- bash bin/host-gate-hook.sh
        language: system
        stages: [pre-commit]
        always_run: true
        pass_filenames: false
YAML
git add .gitignore .pre-commit-config.yaml bin specs
git commit -q -m "Initialize host gate fixture"
printf 'implemented by mock pi\n' > loom-smoke-result.txt
git add loom-smoke-result.txt
git commit -q -m "Implement smoke bead"
git verify-commit HEAD

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

test_smoke_runtime_executes_host_hook
test_smoke_runtime_rejects_failing_host_hook

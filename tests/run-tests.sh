#!/usr/bin/env bash
set -euo pipefail

# Container smoke harness for loom.
#
# One happy-path scenario validates host↔container plumbing the
# integration tier cannot reach: a temp `.beads/` is seeded with one ready
# bead labelled `profile:base`, the test image bundles `mock-pi` at a
# known path inside the container, loom invokes `wrix spawn` with
# `WRIX_AGENT=pi` `MOCK_PI_SCENARIO=happy-path`, and the smoke asserts
# the container exits clean with the bead closed.
#
# Spec: specs/tests.md § Container smoke. Per NFR #6 this uses live `bd`,
# never a mock.
#
# Environment inputs (set by the Nix `writeShellApplication` wrapper):
#   LOOM_BIN                 — path to the loom binary (default: `loom` on PATH)
#   LOOM_TEST_IMAGE          — path to a `podman load`-compatible archive
#                              whose ref is LOOM_TEST_IMAGE_REF; the image
#                              must contain `pi` as the mock-pi script.
#   LOOM_TEST_IMAGE_REF      — image ref tag (e.g. localhost/wrix-test:smoke)
#   LOOM_TEST_IMAGE_SOURCE_KIND — wrix image source kind (default: nix-descriptor)
#   LOOM_TEST_PROFILE_CONFIG — immutable wrix ProfileConfig for the mock image
#   LOOM_TEST_PRE_PUSH_CHECKS — canonical repository pre-push wrapper
#   LOOM_TEST_BEADS_BOOTSTRAP — immutable raw Beads for the private bootstrap only
#   LOOM_SMOKE_REPORT_DIR    — native-kernel report role; retain diagnostics and
#                              leave private mounts to whole-VM retirement
#   LOOM_TEST_GIT_POLICY     — script granting smoke repository Git credentials
#   LOOM_TEST_SEED_BEADS     — script seeding spec metadata and ready smoke work
#   LOOM_TEST_LOOP           — script running the bead and checking its result
#   LOOM_TEST_WAIT_DOLT      — bounded startup readiness and failure diagnostics
#   WRIX_PREK_HOOKS          — canonical packaged wrix prek hook directory
#   LOOM_SMOKE_KEEP          — defaults to 1 to preserve the temp workspace after
#                              a failed run for diagnosis
#   LOOM_WRIX_SERVICE_BIN    — profiled wrix launcher used for service setup
#   LOOM_WRIX_SPAWN_BIN      — raw wrix launcher used by SpawnConfig
#   LOOM_TEST_MOCK_PI_PATH   — host path to tests/mock-pi/pi.sh; sourced for
#                              the container build step when the image is
#                              produced inline rather than from a Nix archive.
#
# Exit codes: 0 success, 77 skipped (prereqs missing), non-zero on failure.

START_TS=$(date +%s)

log() {
    printf '[smoke] %s\n' "$1" >&2
}

cleanup() {
    local ec=$?
    if [[ -n "${LOOM_SMOKE_REPORT_DIR:-}" && -d "${WORKSPACE:-}" ]]; then
        if ! (
            cd "$WORKSPACE" || exit 1
            mkdir "$LOOM_SMOKE_REPORT_DIR" || exit 1
            find .loom -type f \( -name '*.log' -o -name '*.jsonl' -o -path '*/.loom/logs/*' -o -path '*/.loom/diagnostics/*' \) \
                -print0 > "$LOOM_SMOKE_REPORT_DIR/files.nul" || exit 1
            while IFS= read -r -d '' file; do
                mkdir -p "$LOOM_SMOKE_REPORT_DIR/$(dirname "$file")" || exit 1
                cp -- "$file" "$LOOM_SMOKE_REPORT_DIR/$file" || exit 1
            done < "$LOOM_SMOKE_REPORT_DIR/files.nul"
        ); then
            log 'could not retain complete smoke diagnostics'
            if [[ "$ec" == 0 ]]; then ec=1; fi
        fi
    fi
    if [[ "${DOLT_SERVICE_STARTED:-0}" == "1" && -n "${WORKSPACE:-}" && -d "$WORKSPACE" ]]; then
        (cd "$WORKSPACE" && "$LOOM_WRIX_SERVICE_BIN" service stop >/dev/null 2>&1) || true # best-effort: cleanup must not mask the test result.
    fi
    if [[ -n "${LOOM_SMOKE_REPORT_DIR:-}" ]]; then
        # The fixed kernel supervisor retires the entire owned VM. Never recurse
        # into still-mounted container trees; retain their private disk instead.
        log "retained workspace in owned native-kernel scope: $SMOKE_ROOT"
    elif [[ "$ec" -ne 0 && "${LOOM_SMOKE_KEEP:-1}" == "1" ]]; then
        log "preserved failed workspace: $SMOKE_ROOT"
    elif [[ -n "${SMOKE_ROOT:-}" && -d "$SMOKE_ROOT" ]]; then
        rm -rf "$SMOKE_ROOT"
    fi
    exit "$ec"
}
trap cleanup EXIT

require() {
    local bin="$1"
    if ! command -v "$bin" >/dev/null 2>&1; then
        log "skip: required binary '$bin' not on PATH"
        exit 77
    fi
}

require bd
require dolt
require git
require jq
require podman

if [[ ! -c /dev/fuse || ! -c /dev/net/tun ]]; then
    log "skip: container runtime devices /dev/fuse and /dev/net/tun are required"
    exit 77
fi

scrub_git_local_env() {
    local vars
    local var
    vars="$(git rev-parse --local-env-vars)"
    while IFS= read -r var; do
        if [[ -n "$var" ]]; then
            unset "$var"
        fi
    done <<<"$vars"
}

LOOM_BIN="${LOOM_BIN:-loom}"
if ! command -v "$LOOM_BIN" >/dev/null 2>&1; then
    log "skip: loom binary '$LOOM_BIN' not on PATH"
    exit 77
fi

if [[ -z "${LOOM_TEST_IMAGE:-}" || -z "${LOOM_TEST_IMAGE_REF:-}" || -z "${LOOM_TEST_PROFILE_CONFIG:-}" ]]; then
    log "error: the Nix smoke wrapper did not supply its image and ProfileConfig"
    exit 1
fi
if [[ -z "${LOOM_WRIX_SERVICE_BIN:-}" || ! -x "$LOOM_WRIX_SERVICE_BIN" ]]; then
    log "error: the Nix smoke wrapper did not supply an executable Wrix service launcher"
    exit 1
fi
if [[ -z "${LOOM_WRIX_SPAWN_BIN:-}" || ! -x "$LOOM_WRIX_SPAWN_BIN" ]]; then
    log "error: the Nix smoke wrapper did not supply an executable Wrix spawn launcher"
    exit 1
fi
LOOM_TEST_IMAGE_SOURCE_KIND="${LOOM_TEST_IMAGE_SOURCE_KIND:-nix-descriptor}"

SMOKE_ROOT="$(mktemp -d -t loom-smoke.XXXXXX)"
export HOME="$SMOKE_ROOT/home"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_RUNTIME_DIR="$SMOKE_ROOT/runtime"
mkdir -m 700 "$XDG_RUNTIME_DIR"
export CONTAINERS_CONF="$SMOKE_ROOT/containers.conf"
printf '[engine]\ncgroup_manager = "cgroupfs"\nevents_logger = "file"\nhooks_dir = ["%s"]\n' \
    "${LOOM_TEST_PROC_HOOKS:?missing immutable restricted proc hook}" > "$CONTAINERS_CONF"
export CONTAINERS_STORAGE_CONF="$SMOKE_ROOT/storage.conf"
printf '[storage]\ndriver = "overlay"\nrunroot = "%s/storage"\ngraphroot = "%s/containers"\n[storage.options.overlay]\nignore_chown_errors = "true"\nmount_program = "%s"\n' \
    "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "${LOOM_TEST_FUSE_OVERLAYFS:?missing immutable overlay helper}" > "$CONTAINERS_STORAGE_CONF"
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
mkdir -m 700 "$HOME"
WORKSPACE="$SMOKE_ROOT/workspace"
ORIGIN="$SMOKE_ROOT/origin.git"
export WRIX_DEPLOY_KEY="$SMOKE_ROOT/deploy-key"
export WRIX_SIGNING_KEY="$SMOKE_ROOT/signing-key"
export WRIX_PI_AUTH_FILE="$SMOKE_ROOT/pi-auth.json"
ssh-keygen -q -t ed25519 -N "" -f "$WRIX_DEPLOY_KEY"
ssh-keygen -q -t ed25519 -N "" -f "$WRIX_SIGNING_KEY"
printf '{}\n' >"$WRIX_PI_AUTH_FILE"
log "workspace: $WORKSPACE"

scrub_git_local_env
git init -q --bare --initial-branch=main "$ORIGIN"
git init -q "$WORKSPACE"
cd "$WORKSPACE"
git config user.email smoke@example.com
git config user.name "Loom Smoke"
git branch -M main
bash "${LOOM_TEST_GIT_POLICY:?missing smoke Git policy helper}"

mkdir -p bin docs specs .loom
cp -R "${LOOM_TEST_NATIVE_SMOKE_FIXTURE:?missing checked smoke definition}/." .
chmod -R u+w "$WORKSPACE"
cp "$LOOM_TEST_PRE_PUSH_CHECKS" bin/pre-push-checks
chmod +x bin/pre-push-checks
install -m 0644 "${LOOM_TEST_SMOKE_SPEC:?missing indexed smoke acceptance}" specs/smoke.md
cat >docs/README.md <<'DOCS'
# Loom Docs

| Spec | Code | Epic | Purpose |
|------|------|------|---------|
| [smoke](../specs/smoke.md) | — | — | Container smoke fixture |
DOCS
cat >.pre-commit-config.yaml <<'YAML'
repos:
  - repo: local
    hooks:
      - id: smoke-noop
        name: smoke fixture no-op
        entry: bin/pre-push-checks --hook-id smoke-noop --hook-entry true -- true
        language: system
        stages: [pre-commit, pre-push]
        always_run: true
        pass_filenames: false
YAML
cat >.gitignore <<'IGNORE'
.loom/
.wrix/
*.gate.lock*
IGNORE
git add .gitignore .pre-commit-config.yaml wrix.toml bin/pre-push-checks docs/README.md specs/smoke.md flake.nix verification.nix result-check.sh loom-smoke-result.txt
git commit -q -m "Initialize smoke workspace"
git remote add origin "$ORIGIN"
git push -q -u origin main

mkdir -p .beads
chmod 700 .beads
(
    data="$SMOKE_ROOT/bootstrap-data"
    socket="$SMOKE_ROOT/bootstrap.sock"
    mkdir -p "$data"
    dolt sql-server --data-dir "$data" --host 127.0.0.1 --port 13307 \
        --socket "$socket" >"$SMOKE_ROOT/bootstrap.log" 2>&1 &
    server_pid=$!
    stop_bootstrap() {
        kill "$server_pid"
        wait "$server_pid" || [[ "$?" == 143 ]]
    }
    trap stop_bootstrap EXIT
    deadline=$((SECONDS + 30))
    while true; do
        remaining=$((deadline - SECONDS))
        if ((remaining <= 0)); then
            log 'error: bootstrap Dolt did not become ready within 30 seconds'
            exit 1
        fi
        if timeout "${remaining}s" dolt --host 127.0.0.1 --port 13307 --user root --password '' --no-tls \
            sql -q 'SELECT 1' >"$SMOKE_ROOT/bootstrap-probe.log" 2>&1 && [[ -S "$socket" ]]; then
            break
        fi
        kill -0 "$server_pid"
        sleep 0.1
    done
    BEADS_DOLT_SERVER_SOCKET="$socket" BEADS_DOLT_AUTO_START=0 \
        "${LOOM_TEST_BEADS_BOOTSTRAP:?missing private bootstrap Beads tool}" init --prefix=smoke --skip-hooks --skip-agents --non-interactive \
            --server --server-socket "$socket" --database smoke >/dev/null
    stop_bootstrap
    trap - EXIT
    rmdir .beads/dolt
    mv "$data" .beads/dolt
)
printf '\nsync-branch: beads\n' >> .beads/config.yaml
# Wrix's first service startup imports this real branch and its Git administration.
BEADS_WORKTREE="$WORKSPACE/.git/beads-worktrees/beads"
git worktree add --quiet --orphan -b beads "$BEADS_WORKTREE"
mkdir -p "$BEADS_WORKTREE/.beads/dolt-remote"
touch "$BEADS_WORKTREE/.beads/.gitkeep"
git -C "$BEADS_WORKTREE" add .beads/.gitkeep
git -C "$BEADS_WORKTREE" commit -q -m "Initialize beads branch"
git -C "$BEADS_WORKTREE" push -q -u origin beads

# Admission belongs only to this fixture's fresh HOME/XDG state, never the operator's.
"$LOOM_WRIX_SERVICE_BIN" workspace initialize
"$LOOM_WRIX_SERVICE_BIN" workspace acknowledge-scope --version 1
"$LOOM_WRIX_SERVICE_BIN" service start >/dev/null
DOLT_SERVICE_STARTED=1
bash "${LOOM_TEST_WAIT_DOLT:?missing Dolt readiness helper}" "$LOOM_WRIX_SERVICE_BIN" >/dev/null
client_env=$("$LOOM_WRIX_SERVICE_BIN" service dolt client-env)
eval "$client_env"
unset client_env

printf '\n/config.local.yaml\n' >>.beads/.gitignore
chmod 700 .beads
bd config set export.auto false >/dev/null
git add .beads/.gitignore .beads/README.md .beads/config.yaml .beads/metadata.json
git commit -q -m "Configure smoke beads"
git push -q origin main

BEAD_ID=$(bash "$LOOM_TEST_SEED_BEADS")

jq '.profile.env.LOOM_SMOKE_INTERRUPT_ONCE = "1"' \
    "$LOOM_TEST_PROFILE_CONFIG" > "$SMOKE_ROOT/restart-profile.json"
LOOM_TEST_PROFILE_CONFIG="$SMOKE_ROOT/restart-profile.json"

cat >"$WORKSPACE/profile-images.json" <<JSON
{
  "base": {
    "pi": {
      "ref": "${LOOM_TEST_IMAGE_REF}",
      "source": "${LOOM_TEST_IMAGE}",
      "source_kind": "${LOOM_TEST_IMAGE_SOURCE_KIND}",
      "launcher": "${LOOM_WRIX_SPAWN_BIN}",
      "profile_config": "${LOOM_TEST_PROFILE_CONFIG}"
    }
  }
}
JSON

native_key=''
while IFS= read -r line; do
    if [[ "$line" == 'native_key: '* ]]; then native_key="${line#native_key: }"; fi
done < <("$LOOM_WRIX_SERVICE_BIN" workspace inspect --checkout "$WORKSPACE")
[[ "$native_key" =~ ^[a-f0-9]{64}$ ]]
operator_git=$(stat -c '%d:%i' "$WORKSPACE/.git")
membership="loom-smoke-${operator_git//:/-}"
workspace_adapter="$SMOKE_ROOT/workspace-membership"
{
    printf '#!%s\nset -euo pipefail\n' "${LOOM_TEST_SHELL:?missing immutable shell}"
    printf 'export PATH=%q\noperator=%q\noperator_git=%q\nmembership=%q\nnative_key=%q\nwrix=%q\n' \
        "${LOOM_TEST_MEMBERSHIP_PATH:?missing membership tools}" "$WORKSPACE" "$operator_git" \
        "$membership" "$native_key" "$LOOM_WRIX_SERVICE_BIN"
    cat "${LOOM_TEST_MEMBERSHIP_SCRIPT:?missing membership policy}"
} > "$workspace_adapter"
chmod 700 "$workspace_adapter"
"$LOOM_WRIX_SERVICE_BIN" workspace integration select --slot loom-smoke --executable "$workspace_adapter" >/dev/null
"$LOOM_WRIX_SERVICE_BIN" workspace admit --integration loom-smoke --expect-membership "$membership" \
    --checkout "$WORKSPACE" --retain "$native_key" >/dev/null
"$LOOM_BIN" --workspace "$WORKSPACE" init --rebuild --workspace-associator "$workspace_adapter" >/dev/null
log "seeded bead: $BEAD_ID"

unset WRIX_AGENT
bash "$LOOM_TEST_LOOP" "$LOOM_BIN" "$WORKSPACE" "$BEAD_ID" "$START_TS" --interrupt-once

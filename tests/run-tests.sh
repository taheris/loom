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
    if [[ -n "${SMOKE_ROOT:-}" && -d "$SMOKE_ROOT" ]]; then
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
LOOM_TEST_IMAGE_SOURCE_KIND="${LOOM_TEST_IMAGE_SOURCE_KIND:-nix-descriptor}"

SMOKE_ROOT="$(mktemp -d -t loom-smoke.XXXXXX)"
WORKSPACE="$SMOKE_ROOT/workspace"
ORIGIN="$SMOKE_ROOT/origin.git"
log "workspace: $WORKSPACE"

scrub_git_local_env
git init -q --bare "$ORIGIN"
git init -q "$WORKSPACE"
cd "$WORKSPACE"
git config user.email smoke@example.com
git config user.name "Loom Smoke"
git branch -M main

mkdir -p docs specs .loom
cat >specs/smoke.md <<'SPEC'
# Smoke

Container-smoke fixture spec.
SPEC
cat >docs/README.md <<'DOCS'
# Loom Docs

| Spec | Code | Epic | Purpose |
|------|------|------|---------|
| [smoke.md](../specs/smoke.md) | — | — | Container smoke fixture |
DOCS
git add docs/README.md specs/smoke.md
git commit -q -m "Initialize smoke workspace"
git remote add origin "$ORIGIN"
git push -q -u origin main

bd init --prefix=smoke >/dev/null
BASE_COMMIT=$(git rev-parse HEAD)
MOLECULE_ID=$(bd create "smoke molecule" \
    --description "container smoke molecule" \
    --type=epic --priority=2 \
    --labels="loom:spec,spec:smoke,profile:base" \
    --metadata "{\"loom.base_commit\":\"$BASE_COMMIT\"}" \
    --silent)

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

"$LOOM_BIN" --workspace "$WORKSPACE" init --rebuild >/dev/null
"$LOOM_BIN" --workspace "$WORKSPACE" use smoke >/dev/null

BEAD_ID=$(bd create "smoke happy-path" \
    --description "container smoke: pi happy-path" \
    --type=task --priority=2 \
    --labels="spec:smoke,profile:base" \
    --parent="$MOLECULE_ID" \
    --silent)
log "seeded bead: $BEAD_ID"

unset WRIX_AGENT
set +e
LOOM_PROFILES_MANIFEST="$WORKSPACE/profile-images.json" \
"$LOOM_BIN" --workspace "$WORKSPACE" --host-key --agent pi loop "$BEAD_ID"
RC=$?
set -e

if [[ "$RC" -ne 0 ]]; then
    log "loom loop $BEAD_ID failed with exit $RC"
    exit 1
fi

if ! STATUS=$(bd show "$BEAD_ID" --json | jq -er 'if type == "array" then .[0].status else .status end'); then
    log "failed to read bead $BEAD_ID status"
    exit 1
fi
if [[ "$STATUS" != "closed" ]]; then
    log "bead $BEAD_ID did not close: status=$STATUS"
    exit 1
fi
log "bead $BEAD_ID closed"

END_TS=$(date +%s)
ELAPSED=$((END_TS - START_TS))
log "elapsed: ${ELAPSED}s"
if [[ "$ELAPSED" -gt 30 ]]; then
    log "smoke exceeded 30s wall-time budget: ${ELAPSED}s"
    exit 1
fi

log "ok"

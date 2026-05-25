#!/usr/bin/env bash
# Container smoke harness for loom.
#
# One happy-path scenario validates host↔container plumbing the
# integration tier cannot reach: a temp `.beads/` is seeded with one ready
# bead labelled `profile:base`, the test image bundles `mock-pi` at a
# known path inside the container, loom invokes `wrapix spawn` with
# `WRAPIX_AGENT=pi` `MOCK_PI_SCENARIO=happy-path`, and the smoke asserts
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
#   LOOM_TEST_IMAGE_REF      — image ref tag (e.g. localhost/wrapix-test:smoke)
#   LOOM_TEST_MOCK_PI_PATH   — host path to tests/mock-pi/pi.sh; sourced for
#                              the container build step when the image is
#                              produced inline rather than from a Nix archive.
#
# Exit codes: 0 success, 77 skipped (prereqs missing), non-zero on failure.

set -euo pipefail

START_TS=$(date +%s)

log() {
    printf '[smoke] %s\n' "$1" >&2
}

cleanup() {
    local ec=$?
    if [[ -n "${WORKSPACE:-}" && -d "$WORKSPACE" ]]; then
        rm -rf "$WORKSPACE"
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
require podman

LOOM_BIN="${LOOM_BIN:-loom}"
if ! command -v "$LOOM_BIN" >/dev/null 2>&1; then
    log "skip: loom binary '$LOOM_BIN' not on PATH"
    exit 77
fi

if [[ -z "${LOOM_TEST_IMAGE:-}" || -z "${LOOM_TEST_IMAGE_REF:-}" ]]; then
    log "skip: LOOM_TEST_IMAGE and LOOM_TEST_IMAGE_REF must be set"
    log "      (the Nix wrapper builds the test image and points these here)"
    exit 77
fi

WORKSPACE="$(mktemp -d -t loom-smoke.XXXXXX)"
log "workspace: $WORKSPACE"

cd "$WORKSPACE"
git init -q
git commit -q --allow-empty -m "smoke init"

bd init --prefix=smoke >/dev/null

mkdir -p specs .wrapix/loom
cat >specs/smoke.md <<'SPEC'
# Smoke

Container-smoke fixture spec.
SPEC

cat >"$WORKSPACE/profile-images.json" <<JSON
{
  "base": {
    "ref": "${LOOM_TEST_IMAGE_REF}",
    "source": "${LOOM_TEST_IMAGE}"
  }
}
JSON

"$LOOM_BIN" --workspace "$WORKSPACE" init >/dev/null
"$LOOM_BIN" --workspace "$WORKSPACE" use smoke >/dev/null

BEAD_ID=$(bd create "smoke happy-path" \
    --description "container smoke: pi happy-path" \
    --type=task --priority=2 \
    --labels="spec:smoke,profile:base" \
    --silent)
log "seeded bead: $BEAD_ID"

set +e
LOOM_PROFILES_MANIFEST="$WORKSPACE/profile-images.json" \
WRAPIX_AGENT=pi \
MOCK_PI_SCENARIO=happy-path \
"$LOOM_BIN" --workspace "$WORKSPACE" -A pi run --once
RC=$?
set -e

if [[ $RC -ne 0 ]]; then
    log "loom run --once failed with exit $RC"
    exit 1
fi

STATUS=$(bd show "$BEAD_ID" --json 2>/dev/null | grep -oE '"status"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | sed 's/.*"\([^"]*\)"$/\1/')
if [[ "$STATUS" != "closed" ]]; then
    log "bead $BEAD_ID did not close: status=$STATUS"
    exit 1
fi
log "bead $BEAD_ID closed"

END_TS=$(date +%s)
ELAPSED=$((END_TS - START_TS))
log "elapsed: ${ELAPSED}s"
if [[ $ELAPSED -gt 30 ]]; then
    log "smoke exceeded 30s wall-time budget: ${ELAPSED}s"
    exit 1
fi

log "ok"

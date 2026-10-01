#!/usr/bin/env bash
set -euo pipefail

# Run the seeded smoke bead and check closure before reporting elapsed time.
LOOM_BIN="${1:?loom binary is required}"
WORKSPACE="${2:?workspace is required}"
BEAD_ID="${3:?bead id is required}"
START_TS="${4:?start timestamp is required}"

log() {
    printf '[smoke] %s\n' "$1" >&2
}

set +e
LOOM_PROFILES_MANIFEST="$WORKSPACE/profile-images.json" \
"$LOOM_BIN" --workspace "$WORKSPACE" --agent pi loop "$BEAD_ID"
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
    log "warning: smoke exceeded 30s soft target: ${ELAPSED}s (advisory)"
fi

log "ok"

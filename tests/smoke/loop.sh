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

if [[ "${5:-}" == --interrupt-once ]]; then
    first_log="$WORKSPACE/.loom/smoke-interrupted-driver.log"
    LOOM_PROFILES_MANIFEST="$WORKSPACE/profile-images.json" \
        "$LOOM_BIN" --workspace "$WORKSPACE" --agent pi loop "$BEAD_ID" > "$first_log" 2>&1 &
    first_pid=$!
    for _attempt in $(seq 1 120); do
        if grep -Fq 'smoke worker awaits driver interruption' "$first_log"; then
            break
        fi
        if ! kill -0 "$first_pid" 2>/dev/null; then
            cat "$first_log" >&2
            log 'driver exited before interruption readiness'
            exit 1
        fi
        sleep 1
    done
    if ! grep -Fq 'smoke worker awaits driver interruption' "$first_log"; then
        kill -KILL "$first_pid"
        wait "$first_pid" || true # best-effort: retain the readiness failure during child cleanup.
        cat "$first_log" >&2
        log 'driver interruption readiness timed out'
        exit 1
    fi
    kill -KILL "$first_pid"
    if wait "$first_pid"; then
        log 'killed driver unexpectedly succeeded'
        exit 1
    else
        first_rc=$?
    fi
    [[ "$first_rc" == 137 && -d "$WORKSPACE/.loom/beads/$BEAD_ID/.git" ]]
    profile=$(jq -er '.base.pi.profile_config' "$WORKSPACE/profile-images.json")
    jq '.profile.env.LOOM_SMOKE_INTERRUPT_ONCE = "0"' "$profile" > "$profile.next"
    mv "$profile.next" "$profile"
    log 'interrupted driver; restarting against the preserved worker checkout'
fi

set +e
LOOM_PROFILES_MANIFEST="$WORKSPACE/profile-images.json" \
timeout --kill-after=10s 600s "$LOOM_BIN" --workspace "$WORKSPACE" --agent pi loop "$BEAD_ID"
RC=$?
set -e

if [[ "$RC" -ne 0 ]]; then
    log "loom loop $BEAD_ID failed with exit $RC"
    exit 1
fi

if ! BEAD_JSON=$(bd show "$BEAD_ID" --json) ||
    ! STATUS=$(jq -er 'if type == "array" then .[0].status else .status end' <<< "$BEAD_JSON"); then
    log "failed to read bead $BEAD_ID status"
    exit 1
fi
if [[ "$STATUS" != "closed" ]]; then
    log "bead $BEAD_ID did not close: status=$STATUS"
    printf '%s\n' "$BEAD_JSON" >&2
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

#!/usr/bin/env bash
set -euo pipefail

# Run the seeded smoke bead and check closure before reporting elapsed time.
LOOM_BIN="${1:?loom binary is required}"
WORKSPACE="${2:?workspace is required}"
BEAD_ID="${3:?bead id is required}"
START_TS="${4:?start timestamp is required}"
mkdir -p "$WORKSPACE/.loom"

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

if [[ "${5:-}" == --interrupt-once ]]; then
    verifier_git="$WORKSPACE/.loom/integration/.git"
    verifier_hold="$verifier_git/smoke-verifier-hold"
    verifier_entered="$verifier_git/smoke-verifier-entered"
    printf 'hold\n' > "$verifier_hold"
    printf 'outage\n' > "$verifier_git/smoke-verifier-unavailable"
    verifier_log="$WORKSPACE/.loom/smoke-verifier-interrupted-driver.log"
    LOOM_PROFILES_MANIFEST="$WORKSPACE/profile-images.json" \
        "$LOOM_BIN" --workspace "$WORKSPACE" --agent pi loop "$BEAD_ID" > "$verifier_log" 2>&1 &
    verifier_driver=$!
    for _attempt in $(seq 1 120); do
        if [[ -f "$verifier_entered" ]]; then
            break
        fi
        if ! kill -0 "$verifier_driver" 2>/dev/null; then
            cat "$verifier_log" >&2
            log 'driver exited before actual verifier interruption point'
            exit 1
        fi
        sleep 1
    done
    if [[ ! -f "$verifier_entered" ]]; then
        kill -KILL "$verifier_driver"
        wait "$verifier_driver" || true # best-effort: preserve readiness failure during owned-child cleanup.
        cat "$verifier_log" >&2
        log 'actual verifier interruption readiness timed out'
        exit 1
    fi
    source_head=$(git -C "$WORKSPACE/.loom/beads/$BEAD_ID" rev-parse HEAD)
    read -r verifier_child verifier_parent < "$verifier_entered"
    kill -KILL "$verifier_driver"
    if wait "$verifier_driver"; then
        log 'driver killed during verification unexpectedly succeeded'
        exit 1
    else
        verifier_rc=$?
    fi
    [[ "$verifier_rc" == 137 ]]
    rm "$verifier_hold"
    for _attempt in $(seq 1 30); do
        if ! kill -0 "$verifier_child" 2>/dev/null && ! kill -0 "$verifier_parent" 2>/dev/null; then
            break
        fi
        sleep 1
    done
    if kill -0 "$verifier_child" 2>/dev/null || kill -0 "$verifier_parent" 2>/dev/null; then
        log 'owned verifier did not exit after bounded release; refusing concurrent restart'
        exit 1
    fi
    log 'interrupted driver during real verification; restarting its persisted candidate'
    outage_log="$WORKSPACE/.loom/smoke-verifier-exhausted-driver.log"
    if LOOM_PROFILES_MANIFEST="$WORKSPACE/profile-images.json" \
        timeout --kill-after=10s 120s "$LOOM_BIN" --workspace "$WORKSPACE" --agent pi loop "$BEAD_ID" > "$outage_log" 2>&1; then
        outage_rc=0
    else
        outage_rc=$?
    fi
    if [[ "$outage_rc" != 0 && "$outage_rc" != 1 ]]; then
        cat "$outage_log" >&2
        log 'verifier outage did not finish within its attempt budget'
        exit 1
    fi
    grep -F 'blocked 1' "$outage_log"
    bd show "$BEAD_ID" --json | jq -e \
        'if type == "array" then .[0] else . end | .status == "blocked" and (.labels | index("loom:infra") != null)' >/dev/null
    jq -se '[.[] | select(.state == "unavailable" and .coverage_admitted == false)] | length == 3' \
        "$WORKSPACE/.loom/integration/.loom/logs/gate/verification-attempts.jsonl" >/dev/null
    blocker=$(bd create --title 'Isolated prerequisite during verifier outage' --type task --silent)
    bd dep add "$BEAD_ID" "$blocker"
    dependency_log="$WORKSPACE/.loom/smoke-dependency-held-driver.log"
    if LOOM_PROFILES_MANIFEST="$WORKSPACE/profile-images.json" \
        timeout --kill-after=10s 120s "$LOOM_BIN" --workspace "$WORKSPACE" --agent pi loop "$BEAD_ID" > "$dependency_log" 2>&1; then
        dependency_rc=0
    else
        dependency_rc=$?
    fi
    if [[ "$dependency_rc" != 0 && "$dependency_rc" != 1 ]]; then
        cat "$dependency_log" >&2
        log 'dependency-held recovery did not finish within its budget'
        exit 1
    fi
    grep -F 'blocked 1' "$dependency_log"
    if grep -Fq 'dispatching agent against per-bead workspace' "$dependency_log"; then
        log 'dependency hold redispatched completed source'
        exit 1
    fi
    [[ "$(git -C "$WORKSPACE/.loom/beads/$BEAD_ID" rev-parse HEAD)" == "$source_head" ]]
    [[ "$(git -C "$WORKSPACE" ls-remote origin refs/heads/main | cut -f1)" != "$source_head" ]]
    bd show "$BEAD_ID" --json | jq -e --arg blocker "$blocker" \
        'if type == "array" then .[0] else . end | .dependencies | any(.id == $blocker and .status == "open" and .dependency_type == "blocks")' >/dev/null
    bd close "$blocker" --reason 'The isolated fixture completed its prerequisite automatically'
    rm "$verifier_git/smoke-verifier-unavailable"
    log 'native-prerequisite-hold-survived-verifier-restart-without-publication'
fi

set +e
LOOM_PROFILES_MANIFEST="$WORKSPACE/profile-images.json" \
timeout --kill-after=10s 600s "$LOOM_BIN" --workspace "$WORKSPACE" --agent pi loop "$BEAD_ID" > "$WORKSPACE/.loom/smoke-final-driver.log" 2>&1
RC=$?
set -e
cat "$WORKSPACE/.loom/smoke-final-driver.log"
if [[ "$RC" -ne 0 ]]; then
    log "loom loop $BEAD_ID failed with exit $RC"
    exit 1
fi

if [[ -n "${source_head:-}" ]]; then
    restored_head=$(git -C "$WORKSPACE/.loom/beads/$BEAD_ID" rev-parse HEAD)
    if [[ "$restored_head" != "$source_head" ]]; then
        log "verification restart changed completed source: $source_head -> $restored_head"
        exit 1
    fi
    if grep -Fq 'dispatching agent against per-bead workspace' "$WORKSPACE/.loom/smoke-final-driver.log"; then
        log 'verification restart redispatched implementation'
        exit 1
    fi
    events=("$WORKSPACE/.git/loom-verification-continuations/"*.json)
    latest="${events[${#events[@]}-1]}"
    if ! jq -e --arg bead "$BEAD_ID" '.bead == $bead and .phase == "resolved"' "$latest" >/dev/null; then
        log 'restart did not finish its publication continuation'
        cat "$latest" >&2
        exit 1
    fi
    published_head=$(git -C "$WORKSPACE/.loom/integration" rev-parse HEAD)
    remote_head=$(git -C "$WORKSPACE" ls-remote origin refs/heads/main | cut -f1)
    if [[ "$published_head" != "$remote_head" ]]; then
        log "candidate was not published: integration=$published_head origin=$remote_head"
        exit 1
    fi
    log 'verifier-restart-resumed-without-source-redispatch-and-published'

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

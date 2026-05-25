#!/usr/bin/env bash
# Mock claude binary for loom-agent tests and the container smoke runner.
#
# Reads stream-json on stdin, emits stream-json on stdout. The first
# argument selects a behavior mode used by the unit tests in
# loom-agent/src/claude/backend.rs:
#
#   steering       — emit one assistant turn, wait for a steer message on
#                    stdin, emit a second assistant turn that echoes the
#                    steer payload, then emit result/success and exit.
#
#   ignore-stdin   — emit result/success, then ignore SIGTERM and stdin
#                    close so the test exercises the SIGTERM → SIGKILL
#                    escalation in the shutdown watchdog.
#
#   happy-path     — system → assistant → result/success. Used by the
#                    container smoke runner; covers the minimum
#                    single-turn lifecycle.
#
# Modes are deliberately small — every mode is shaped to exactly one
# Rust test (or the smoke runner). The script is not a general-purpose
# claude emulator.
set -euo pipefail

MODE="${1:-default}"

# stream-json envelopes are JSONL: one complete object per line. unbuffer
# stdout (stdbuf -oL) so the consumer reads each line as soon as it is
# written rather than waiting on the default block-buffered flush.
exec 1> >(stdbuf -oL cat)

emit() {
    printf '%s\n' "$1"
}

emit_system_init() {
    emit '{"type":"system","subtype":"init","session_id":"mock-claude-session"}'
}

emit_assistant_text() {
    local text="$1"
    # Escape backslashes and double-quotes so the embedded text survives
    # round-tripping through bash → JSON → serde.
    text="${text//\\/\\\\}"
    text="${text//\"/\\\"}"
    emit "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"${text}\"}]}}"
}

emit_result_success() {
    emit '{"type":"result","subtype":"success","total_cost_usd":0.0,"duration_ms":1,"num_turns":1,"is_error":false}'
}

case "$MODE" in
    steering)
        # Read first user message (initial prompt). We don't parse it —
        # the test only cares that the mock emits its turn after seeing
        # any input.
        IFS= read -r _initial
        emit_assistant_text "first turn response"

        # Wait for the driver's steer message. The test passes a string
        # containing "STEERED_TEXT"; we emit it back so the test can
        # assert the second turn was triggered by the steer.
        IFS= read -r steer_line
        # Pull the content field out via a sloppy regex. jq is not a hard
        # dependency in the wrapix tests env, but stream-json messages
        # use a stable shape so a substring match is sufficient.
        if [[ "$steer_line" == *STEERED_TEXT* ]]; then
            emit_assistant_text "ack STEERED_TEXT"
        else
            emit_assistant_text "ack unknown steer: $steer_line"
        fi

        emit_result_success
        exit 0
        ;;
    ignore-stdin)
        # Drain the driver's initial prompt so the write returns. The
        # mode name refers to ignoring stdin *close* (and the subsequent
        # SIGTERM), not refusing to read at all; constrained sandboxes
        # ship pipes as small as 8 KiB and the encoded prompt is ~8 KiB,
        # so a mock that never reads would deadlock the driver in its
        # prompt write before the watchdog ever ran.
        IFS= read -r _initial || true
        emit_result_success

        # Trap SIGTERM and SIGPIPE so the test's shutdown watchdog must
        # escalate to SIGKILL. SIGKILL is uncatchable.
        trap '' TERM PIPE
        # Loop forever — kernel reaps us via SIGKILL.
        while true; do
            sleep 0.1
        done
        ;;
    happy-path)
        emit_system_init
        # Read the prompt so the smoke runner's stdin write doesn't block
        # when the driver pipe stays open longer than the agent loop.
        IFS= read -r _initial
        emit_assistant_text "ack"
        emit_result_success
        exit 0
        ;;
    *)
        echo "mock-claude: unknown mode: $MODE" >&2
        exit 2
        ;;
esac

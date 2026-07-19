#!/usr/bin/env bash
set -euo pipefail

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
#   interactive-compaction-canary
#                  — verify interactive `claude --settings <file>` loads
#                    the compact SessionStart hook before emitting output.
#
# Modes are deliberately small and single-purpose. A mode may support
# multiple tests of the same wire behavior; this is not a general-purpose
# claude emulator.

MODE="${1:-default}"
CANARY_NONCE="LOOM_COMPACTION_CANARY_NONCE_4f0b3f0f"
POLISH_NO_EDIT_PHRASE="Propose specific edits or findings, but do not apply edits unless explicitly asked to apply them."

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

extract_settings_arg() {
    local settings=""
    while [[ "$#" -gt 0 ]]; do
        case "$1" in
            --settings)
                if [[ "$#" -lt 2 ]]; then
                    echo "mock-claude: --settings requires a path" >&2
                    exit 2
                fi
                settings="$2"
                shift 2
                ;;
            --settings=*)
                settings="${1#--settings=}"
                shift
                ;;
            *)
                shift
                ;;
        esac
    done
    printf '%s\n' "$settings"
}

hook_command_from_settings() {
    local settings_path="$1"
    local line command=""
    while IFS= read -r line; do
        if [[ "$line" =~ \"command\"[[:space:]]*:[[:space:]]*\"([^\"]+)\" ]]; then
            command="${BASH_REMATCH[1]}"
            break
        fi
    done < "$settings_path"
    if [[ -z "$command" ]]; then
        echo "mock-claude: settings missing compact hook command" >&2
        exit 3
    fi
    printf '%s\n' "$command"
}

context_from_settings() {
    local settings_path="$1"
    local hook_cmd workspace
    if [[ ! -f "$settings_path" ]]; then
        echo "mock-claude: settings file not found: $settings_path" >&2
        exit 3
    fi
    hook_cmd="$(hook_command_from_settings "$settings_path")"
    workspace="$PWD"
    case "$settings_path" in
        */.loom/scratch/*/claude-settings.json)
            workspace="${settings_path%%/.loom/scratch/*}"
            ;;
    esac
    if [[ "$hook_cmd" == /* ]]; then
        bash "$hook_cmd"
    else
        (cd "$workspace" && bash "$hook_cmd")
    fi
}

canary_context() {
    if [[ -n "${LOOM_COMPACTION_CANARY_CONTEXT+x}" ]]; then
        printf '%s\n' "$LOOM_COMPACTION_CANARY_CONTEXT"
        return
    fi
    local settings_path
    settings_path="$(extract_settings_arg "$@")"
    if [[ -z "$settings_path" ]]; then
        echo "mock-claude: interactive canary requires --settings" >&2
        exit 3
    fi
    context_from_settings "$settings_path"
}

run_interactive_compaction_canary() {
    local context
    context="$(canary_context "$@")"
    if [[ "$context" != *"$POLISH_NO_EDIT_PHRASE"* ]]; then
        echo "mock-claude: canary missing full polish no-edit definition" >&2
        exit 4
    fi
    if [[ "$context" != *"$CANARY_NONCE"* ]]; then
        echo "mock-claude: canary missing nonce" >&2
        exit 4
    fi
    emit_assistant_text "post-compaction polish canary ok $CANARY_NONCE"
    printf 'LOOM_COMPLETE\n'
}

todo_payload_from_prompt() {
    local prompt_line="$1"
    local work_epic="" head="" fingerprint="" label=""
    if [[ "$prompt_line" =~ Work\ epic\*\*:\ ([^\\]+)\\n-\ \*\*Todo\ head\*\*:\ ([0-9a-f]{40,64})\\n-\ \*\*Todo\ fingerprint\*\*:\ ([0-9a-f]{64}) ]]; then
        work_epic="${BASH_REMATCH[1]}"
        head="${BASH_REMATCH[2]}"
        fingerprint="${BASH_REMATCH[3]}"
    fi
    if [[ -n "$work_epic" && "$prompt_line" =~ \#\#\#\ ([a-z0-9-]+)\\n ]]; then
        label="${BASH_REMATCH[1]}"
        printf 'LOOM_TODO: {"head":"%s","fingerprint":"%s","work_epic":"%s","title":"Mock todo decomposition","specs":[{"label":"%s","outcome":"no-work","reason":"mock audit"}]}' \
            "$head" "$fingerprint" "$work_epic" "$label"
    else
        printf 'LOOM_COMPLETE'
    fi
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
        # dependency in the wrix tests env, but stream-json messages
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
        initial_line=""
        if IFS= read -r initial_line; then
            emit_assistant_text "$(todo_payload_from_prompt "$initial_line")"
        fi
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
    interactive-compaction-canary)
        run_interactive_compaction_canary "${@:2}"
        exit 0
        ;;
    *)
        echo "mock-claude: unknown mode: $MODE" >&2
        exit 2
        ;;
esac

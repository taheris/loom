#!/usr/bin/env bash
set -euo pipefail

# Mock pi binary for loom-agent tests.
#
# Reads JSONL commands on stdin, emits JSONL responses+events on stdout.
# The first argument selects a behavior mode used by the unit tests in
# loom-agent/src/pi/backend.rs and the container smoke runner:
#
#   probe-ok                  — respond to get_state with a valid state
#                               object, then exit. The session-handshake
#                               test asserts loom proceeds past the probe;
#                               nothing more is exchanged.
#   probe-bad-state           — respond to get_state with malformed data
#                               so the driver fails fast.
#   echo-prompt               — probe ok, then echo the prompt payload
#                               as a message_delta so the test can
#                               assert the wire shape.
#   steering                  — probe ok; on prompt emit a first turn +
#                               turn_end; on the next stdin line (steer),
#                               echo its payload back as a message_delta +
#                               turn_end, then agent_end.
#   compaction                — probe ok; on prompt emit compaction_start;
#                               on the next stdin line (re-pin steer),
#                               echo it back as a message_delta + emit
#                               compaction_end + agent_end.
#   set-model                 — probe ok; on the next command (expected
#                               set_model) respond ok and echo the
#                               provider/modelId pair into a later
#                               message_delta after the prompt.
#   set-model-reject          — probe ok; on the next command (expected
#                               set_model) respond with success:false so
#                               the driver hard-fails the handshake.
#   set-thinking-level        — probe ok; on the next command (expected
#                               set_thinking_level) respond ok and echo
#                               the level into a later message_delta
#                               after the prompt.
#   set-thinking-level-reject — probe ok; on the next command (expected
#                               set_thinking_level) respond with
#                               success:false. The loom driver must
#                               log a warn and continue, so the mock
#                               still services a follow-up prompt and
#                               emits agent_end normally.
#   happy-path                — probe ok, prompt → message_delta →
#                               agent_end. Used by the container smoke
#                               and any test that wants the full
#                               single-turn lifecycle.
#   tune-review               — probe ok, require the tracked review-input
#                               canary in the prompt, then emit one finding.
#   interactive-compaction-canary
#                             — verify an interactive Pi launch path delivered
#                               full re-pin context before output.
#   interactive-bridge-canary — probe ok; prompt emits compaction_start,
#                               validates the re-pin steer payload, then
#                               emits LOOM_COMPLETE.
#
# Modes are deliberately small and single-purpose. A mode may support
# multiple tests of the same wire behavior; this is not a general-purpose
# pi emulator.

# pi RPC framing is JSONL: one complete object per line. Re-exec through
# stdbuf so libc stdio writes are line-buffered when wrappers drive this
# script through pipes.
#
# Invoke bash explicitly on $0 instead of relying on the kernel's shebang
# resolver — the script's `#!/usr/bin/env bash` line is not honourable in
# the default nix-build sandbox (`sandbox = true`) where `/usr/bin/env`
# is absent. Running `bash "$0"` reads pi.sh as a plain script and
# bypasses the kernel-level interpreter lookup entirely.
if [[ -z "${MOCK_PI_REEXEC:-}" ]]; then
    export MOCK_PI_REEXEC=1
    exec stdbuf -oL bash "$0" "$@"
fi

MODE="${MOCK_PI_SCENARIO:-${1:-default}}"
CANARY_NONCE="LOOM_COMPACTION_CANARY_NONCE_4f0b3f0f"
POLISH_NO_EDIT_PHRASE="Propose specific edits or findings, but do not apply edits unless explicitly asked to apply them."

emit() {
    printf '%s\n' "$1"
}

# Pull a string field out of a JSON line via sed. Good enough for this
# mock — the protocol values we care about don't contain escaped quotes.
extract_field() {
    local field="$1" line="$2"
    sed -n "s/.*\"${field}\":\"\\([^\"]*\\)\".*/\\1/p" <<<"$line"
}

emit_response_ok() {
    local id="$1" command="$2" data="${3:-null}"
    emit "{\"type\":\"response\",\"id\":\"${id}\",\"command\":\"${command}\",\"success\":true,\"data\":${data}}"
}

emit_response_err() {
    local id="$1" command="$2" error="$3"
    emit "{\"type\":\"response\",\"id\":\"${id}\",\"command\":\"${command}\",\"success\":false,\"error\":\"${error}\"}"
}

emit_message_delta() {
    local text="$1"
    text="${text//\\/\\\\}"
    text="${text//\"/\\\"}"
    text="${text//$'\n'/\\n}"
    emit "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"text\":\"${text}\"}}"
}

emit_turn_end() {
    emit '{"type":"turn_end","message":{},"toolResults":[]}'
}

emit_agent_end() {
    emit '{"type":"agent_end","messages":[]}'
}

emit_compaction_start() {
    emit '{"type":"compaction_start","reason":"threshold"}'
}

emit_compaction_end() {
    emit '{"type":"compaction_end","aborted":false,"reason":"threshold","willRetry":false}'
}

interactive_canary_context() {
    if [[ -n "${LOOM_COMPACTION_CANARY_CONTEXT+x}" ]]; then
        printf '%s\n' "$LOOM_COMPACTION_CANARY_CONTEXT"
        return
    fi
    if [[ "$#" -gt 0 && -f "$1" ]]; then
        cat "$1"
        return
    fi
    echo "mock-pi: interactive canary requires delivered context" >&2
    exit 3
}

run_interactive_compaction_canary() {
    local context
    context="$(interactive_canary_context "${@:2}")"
    if [[ "$context" != *"$POLISH_NO_EDIT_PHRASE"* ]]; then
        echo "mock-pi: canary missing full polish no-edit definition" >&2
        exit 4
    fi
    if [[ "$context" != *"$CANARY_NONCE"* ]]; then
        echo "mock-pi: canary missing nonce" >&2
        exit 4
    fi
    printf 'LOOM_COMPLETE\n'
}

run_interactive_bridge_canary() {
    handle_probe 0
    local _prompt steer_line
    IFS= read -r _prompt
    emit_compaction_start
    IFS= read -r steer_line
    if [[ "$steer_line" != *"$POLISH_NO_EDIT_PHRASE"* ]]; then
        echo "mock-pi: bridge canary missing full polish no-edit definition" >&2
        exit 4
    fi
    if [[ "$steer_line" != *"$CANARY_NONCE"* ]]; then
        echo "mock-pi: bridge canary missing nonce" >&2
        exit 4
    fi
    emit_compaction_end
    emit_message_delta "LOOM_COMPLETE"
    emit_agent_end
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

# Read the first command (must be get_state) and echo either a valid state
# object or malformed data when the first arg is "1".
handle_probe() {
    local bad_state="${1:-0}"
    local probe_line probe_id probe_type data
    IFS= read -r probe_line
    probe_id="$(extract_field id "$probe_line")"
    probe_type="$(extract_field type "$probe_line")"
    if [[ -z "$probe_id" ]]; then
        echo "mock-pi: probe missing id field" >&2
        exit 2
    fi
    if [[ "$probe_type" != "get_state" ]]; then
        emit_response_err "${probe_id:-unknown}" "${probe_type:-unknown}" "expected get_state"
        exit 2
    fi
    if [[ "$bad_state" = "1" ]]; then
        data='{"unexpected":true}'
    else
        data='{"model":null,"thinkingLevel":"medium","isStreaming":false,"isCompacting":false,"messageCount":0,"pendingMessageCount":0}'
    fi
    emit_response_ok "$probe_id" "get_state" "$data"
}

run_probe_ok() {
    handle_probe 0
}

run_happy_path() {
    handle_probe 0
    local prompt_line payload bead_id
    IFS= read -r prompt_line
    if [[ "${LOOM_SMOKE_WORKER:-0}" == "1" && "$prompt_line" == *"Issue: "* ]]; then
        bead_id="$(sed -n 's/.*Issue: \([a-z0-9.-]*\)\\n.*/\1/p' <<<"$prompt_line")"
        if [[ -z "$bead_id" ]]; then
            echo "mock-pi: smoke prompt missing bead id" >&2
            exit 5
        fi
        git config user.email smoke@example.com
        git config user.name "Loom Smoke"
        git config commit.gpgsign false
        printf 'implemented by mock pi\n' > loom-smoke-result.txt
        git add loom-smoke-result.txt
        git commit -q -m "Implement smoke bead"
        bd close "$bead_id" >/dev/null
    fi
    payload="$(todo_payload_from_prompt "$prompt_line")"
    emit_message_delta "$payload"
    emit_agent_end
}

run_tune_review() {
    handle_probe 0
    local prompt_line
    IFS= read -r prompt_line
    if [[ "$prompt_line" != *"TUNE_REVIEW_INPUT_CANARY"* ]]; then
        echo "mock-pi: tune review prompt did not contain fixture input" >&2
        exit 4
    fi
    if [[ "$prompt_line" == *"Use concrete review inputs when evaluating candidate guidance."* ]]; then
        emit_message_delta 'LOOM_FINDING: {"token":"fabricated-result","route":"blocking","bonds":["skills"],"target":{"kind":"Criterion","spec":"skills","anchor":"fixture"},"evidence":"missing test from replay input"}'
    else
        emit_message_delta 'LOOM_COMPLETE'
    fi
    emit_agent_end
}

run_echo_prompt() {
    handle_probe 0
    local prompt_line message
    IFS= read -r prompt_line
    message="$(extract_field message "$prompt_line")"
    emit_message_delta "echo: ${message}"
    emit_agent_end
}

run_steering() {
    handle_probe 0
    local _prompt steer_line steer_msg
    IFS= read -r _prompt
    emit_message_delta "first turn response"
    emit_turn_end

    IFS= read -r steer_line
    steer_msg="$(extract_field message "$steer_line")"
    emit_message_delta "ack ${steer_msg}"
    emit_turn_end
    emit_agent_end
}

run_compaction() {
    handle_probe 0
    local prompt_line steer_line repin_msg payload
    IFS= read -r prompt_line
    emit_compaction_start

    IFS= read -r steer_line
    repin_msg="$(extract_field message "$steer_line")"
    payload="$(todo_payload_from_prompt "$prompt_line")"
    emit_message_delta "repin: ${repin_msg}"
    emit_compaction_end
    emit_message_delta $'\n'"$payload"
    emit_agent_end
}

run_set_model() {
    handle_probe 0
    local set_model_line sm_id sm_type provider model_id _prompt
    IFS= read -r set_model_line
    sm_id="$(extract_field id "$set_model_line")"
    sm_type="$(extract_field type "$set_model_line")"
    provider="$(extract_field provider "$set_model_line")"
    model_id="$(extract_field modelId "$set_model_line")"
    if [[ "$sm_type" != "set_model" ]]; then
        emit_response_err "${sm_id:-unknown}" "${sm_type:-unknown}" "expected set_model"
        return
    fi
    emit_response_ok "$sm_id" "set_model"

    IFS= read -r _prompt
    emit_message_delta "model:${provider}:${model_id}"
    emit_agent_end
}

run_set_model_reject() {
    handle_probe 0
    local set_model_line sm_id sm_type
    IFS= read -r set_model_line
    sm_id="$(extract_field id "$set_model_line")"
    sm_type="$(extract_field type "$set_model_line")"
    if [[ "$sm_type" != "set_model" ]]; then
        emit_response_err "${sm_id:-unknown}" "${sm_type:-unknown}" "expected set_model"
        return
    fi
    emit_response_err "$sm_id" "set_model" "model unavailable"
}

run_set_thinking_level() {
    handle_probe 0
    local stl_line stl_id stl_type level _prompt
    IFS= read -r stl_line
    stl_id="$(extract_field id "$stl_line")"
    stl_type="$(extract_field type "$stl_line")"
    level="$(extract_field level "$stl_line")"
    if [[ "$stl_type" != "set_thinking_level" ]]; then
        emit_response_err "${stl_id:-unknown}" "${stl_type:-unknown}" "expected set_thinking_level"
        return
    fi
    emit_response_ok "$stl_id" "set_thinking_level"

    IFS= read -r _prompt
    emit_message_delta "thinking:${level}"
    emit_agent_end
}

run_set_thinking_level_reject() {
    handle_probe 0
    local stl_line stl_id stl_type level _prompt
    IFS= read -r stl_line
    stl_id="$(extract_field id "$stl_line")"
    stl_type="$(extract_field type "$stl_line")"
    level="$(extract_field level "$stl_line")"
    if [[ "$stl_type" != "set_thinking_level" ]]; then
        emit_response_err "${stl_id:-unknown}" "${stl_type:-unknown}" "expected set_thinking_level"
        return
    fi
    emit_response_err "$stl_id" "set_thinking_level" "unsupported by provider"

    IFS= read -r _prompt
    emit_message_delta "thinking-rejected:${level}"
    emit_agent_end
}

case "$MODE" in
    probe-ok)
        run_probe_ok
        ;;
    probe-bad-state)
        handle_probe 1
        ;;
    echo-prompt)
        run_echo_prompt
        ;;
    steering)
        run_steering
        ;;
    compaction)
        run_compaction
        ;;
    set-model)
        run_set_model
        ;;
    set-model-reject)
        run_set_model_reject
        ;;
    set-thinking-level)
        run_set_thinking_level
        ;;
    set-thinking-level-reject)
        run_set_thinking_level_reject
        ;;
    happy-path)
        run_happy_path
        ;;
    tune-review)
        run_tune_review
        ;;
    interactive-compaction-canary)
        run_interactive_compaction_canary "$@"
        ;;
    interactive-bridge-canary)
        run_interactive_bridge_canary
        ;;
    *)
        echo "mock-pi: unknown mode: $MODE" >&2
        exit 2
        ;;
esac

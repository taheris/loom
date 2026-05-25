#!/usr/bin/env bash
# Mock pi binary for loom-agent tests.
#
# Reads JSONL commands on stdin, emits JSONL responses+events on stdout.
# The first argument selects a behavior mode used by the unit tests in
# loom-agent/src/pi/backend.rs and the container smoke runner:
#
#   probe-ok                  — respond to get_commands with the full
#                               required command set, then exit. The
#                               session-handshake test asserts loom
#                               proceeds past the probe; nothing more
#                               is exchanged.
#   probe-missing-set-model   — respond to get_commands omitting
#                               'set_model' so the driver fails fast.
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
#   hang-probe                — read the get_commands line and then sleep
#                               forever without responding. Drives the
#                               HandshakeTimeout path in spawn_with_handshake.
#   stall-mid-session         — probe ok, prompt acked with one
#                               message_delta, then sleep forever without
#                               emitting agent_end. Drives the run_agent
#                               stall heartbeat path.
#
# Modes are deliberately small — every mode is shaped to exactly one
# Rust test (or the smoke runner). The script is not a general-purpose
# pi emulator.
set -euo pipefail

# pi RPC framing is JSONL: one complete object per line. Re-exec through
# stdbuf so libc stdio writes line-buffered without spawning a process-
# substitution subshell that survives our parent's SIGKILL — the
# `hang-probe` / `stall-mid-session` modes are killed externally and any
# leftover `cat` subshell would keep the test's stderr pipe open and
# cause `Command::output()` to hang forever.
#
# Invoke bash explicitly on $0 instead of relying on the kernel's shebang
# resolver — the script's `#!/usr/bin/env bash` line is not honourable in
# the default nix-build sandbox (`sandbox = true`) where `/usr/bin/env`
# is absent. Running `bash "$0"` reads pi.sh as a plain script and
# bypasses the kernel-level interpreter lookup entirely.
if [ -z "${MOCK_PI_REEXEC:-}" ]; then
    export MOCK_PI_REEXEC=1
    exec stdbuf -oL bash "$0" "$@"
fi

MODE="${1:-default}"

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

# Read the first command (must be get_commands) and either echo a full
# command set, or omit set_model when the first arg is "1".
handle_probe() {
    local missing_set_model="${1:-0}"
    local probe_line probe_id data
    IFS= read -r probe_line
    probe_id="$(extract_field id "$probe_line")"
    if [ -z "$probe_id" ]; then
        echo "mock-pi: probe missing id field" >&2
        exit 2
    fi
    if [ "$missing_set_model" = "1" ]; then
        data='["prompt","steer","abort"]'
    else
        data='["prompt","steer","abort","set_model","compact","get_session_stats"]'
    fi
    emit_response_ok "$probe_id" "get_commands" "$data"
}

run_probe_ok() {
    handle_probe 0
}

run_happy_path() {
    handle_probe 0
    local _prompt
    IFS= read -r _prompt
    emit_message_delta "LOOM_COMPLETE"
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
    local _prompt steer_line repin_msg
    IFS= read -r _prompt
    emit_compaction_start

    IFS= read -r steer_line
    repin_msg="$(extract_field message "$steer_line")"
    emit_message_delta "repin: ${repin_msg}"
    emit_compaction_end
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
    if [ "$sm_type" != "set_model" ]; then
        emit_response_err "${sm_id:-unknown}" "${sm_type:-unknown}" "expected set_model"
        return
    fi
    emit_response_ok "$sm_id" "set_model"

    IFS= read -r _prompt
    emit_message_delta "model:${provider}:${model_id}"
    emit_agent_end
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

# Read the probe line, then sleep forever — the loom side must surface
# HandshakeTimeout instead of blocking on the unanswered probe. `exec
# sleep` so SIGKILL from the loom side hits the PID still holding the
# inherited stderr fd; otherwise the sleep would survive bash's death,
# keep the test's stderr pipe open, and `Command::output()` would never
# return.
run_hang_probe() {
    local probe_line
    IFS= read -r probe_line
    : "$probe_line"
    exec sleep 3600
}

# Probe ok, ack the first prompt with a single message_delta, then sleep
# without emitting agent_end. The loom event loop must keep running and
# emit the stall heartbeat warn! line; the test kills this process after
# the warning lands. See `run_hang_probe` for the `exec sleep` rationale.
run_stall_mid_session() {
    handle_probe 0
    local _prompt
    IFS= read -r _prompt
    emit_message_delta "ack"
    exec sleep 3600
}

case "$MODE" in
    probe-ok)
        run_probe_ok
        ;;
    probe-missing-set-model)
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
    set-thinking-level)
        run_set_thinking_level
        ;;
    set-thinking-level-reject)
        run_set_thinking_level_reject
        ;;
    happy-path)
        run_happy_path
        ;;
    hang-probe)
        run_hang_probe
        ;;
    stall-mid-session)
        run_stall_mid_session
        ;;
    *)
        echo "mock-pi: unknown mode: $MODE" >&2
        exit 2
        ;;
esac

#!/usr/bin/env bash
# Dedicated fixture for the `loom inbox chat` Pi RPC bridge follow-up path.
# It accepts one probe, one initial prompt, and one post-completion prompt.
set -euo pipefail

emit() {
    printf '%s\n' "$1"
}

extract_field() {
    local field="$1" line="$2"
    sed -n "s/.*\"${field}\":\"\\([^\"]*\\)\".*/\\1/p" <<<"$line"
}

emit_response_ok() {
    local id="$1" command="$2" data="${3:-null}"
    emit "{\"type\":\"response\",\"id\":\"${id}\",\"command\":\"${command}\",\"success\":true,\"data\":${data}}"
}

emit_idless_prompt_ack() {
    emit '{"type":"response","command":"prompt","success":true}'
}

emit_message_delta() {
    local text="$1"
    text="${text//\\/\\\\}"
    text="${text//\"/\\\"}"
    text="${text//$'\n'/\\n}"
    emit "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"text\":\"${text}\"}}"
}

emit_agent_end() {
    emit '{"type":"agent_end","messages":[]}'
}

handle_probe() {
    local probe_line probe_id probe_type
    if ! IFS= read -r probe_line; then
        echo "pi-followup: expected get_state probe" >&2
        exit 2
    fi
    probe_id="$(extract_field id "$probe_line")"
    probe_type="$(extract_field type "$probe_line")"
    if [[ -z "$probe_id" ]]; then
        echo "pi-followup: probe missing id field" >&2
        exit 2
    fi
    if [[ "$probe_type" != "get_state" ]]; then
        echo "pi-followup: expected get_state, got ${probe_type:-missing}" >&2
        exit 2
    fi
    emit_response_ok "$probe_id" "get_state" '{"model":null,"thinkingLevel":"medium","isStreaming":false,"isCompacting":false,"messageCount":0,"pendingMessageCount":0}'
}

main() {
    if [[ "$#" -ne 0 ]]; then
        echo "pi-followup: fixture does not accept modes" >&2
        exit 2
    fi

    handle_probe

    local initial_prompt reply_line reply_type reply_msg
    if ! IFS= read -r initial_prompt; then
        echo "pi-followup: expected initial prompt" >&2
        exit 5
    fi
    : "$initial_prompt"
    emit_message_delta "Please answer before I finish."
    emit_agent_end

    if ! IFS= read -r reply_line; then
        echo "pi-followup: expected one follow-up prompt" >&2
        exit 5
    fi
    reply_type="$(extract_field type "$reply_line")"
    reply_msg="$(extract_field message "$reply_line")"
    if [[ "$reply_type" != "prompt" ]]; then
        echo "pi-followup: expected follow-up reply as prompt, got ${reply_type:-missing}" >&2
        exit 5
    fi
    if [[ "$reply_msg" != *"please finish"* ]]; then
        echo "pi-followup: follow-up prompt missing user reply" >&2
        exit 5
    fi
    emit_idless_prompt_ack
    emit_message_delta $'\nLOOM_COMPLETE'
    emit_agent_end
}

main "$@"

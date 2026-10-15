#!/usr/bin/env bash
set -euo pipefail

field() {
    local name="$1" line="$2"
    sed -n "s/.*\"${name}\":\"\\([^\"]*\\)\".*/\\1/p" <<< "$line"
}

IFS= read -r probe
probe_id=$(field id "$probe")
[[ -n "$probe_id" && "$(field type "$probe")" == get_state ]]
printf '{"type":"response","id":"%s","command":"get_state","success":true,"data":{"model":null,"thinkingLevel":"medium","isStreaming":false,"isCompacting":false,"messageCount":0,"pendingMessageCount":0}}\n' "$probe_id"

IFS= read -r prompt
if [[ "$(field type "$prompt")" == set_thinking_level ]]; then
    printf '{"type":"response","id":"%s","command":"set_thinking_level","success":true,"data":null}\n' "$(field id "$prompt")"
    IFS= read -r prompt
fi
[[ "$(field type "$prompt")" == prompt ]]
printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","text":"smoke worker awaits driver interruption"}}'
while IFS= read -r _command; do
    :
done

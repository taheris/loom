#!/usr/bin/env bash
set -euo pipefail

receipt="$1"
terminal_event="$2"
IFS= read -r probe
if [[ "$probe" != *'"type":"get_state"'* ]]; then
    printf 'expected get_state probe\n' >&2
    exit 2
fi
id="${probe#*\"id\":\"}"
id="${id%%\"*}"
printf '{"type":"response","id":"%s","command":"get_state","success":true,"data":{"isStreaming":false,"isCompacting":false,"messageCount":0,"pendingMessageCount":0}}\n' "$id"
IFS= read -r prompt
if [[ "$prompt" != *'"type":"prompt"'* ]]; then
    printf 'expected initial prompt\n' >&2
    exit 2
fi
printf '%s\n' "$prompt" > "$receipt/prompt.json"
printf '{"type":"compaction_start","reason":"threshold"}\n'
IFS= read -r steer
if [[ "$steer" != *'"type":"steer"'* ]]; then
    printf 'expected compaction re-pin steer\n' >&2
    exit 2
fi
printf '%s\n' "$steer" > "$receipt/steer.json"
printf '{"type":"compaction_end","aborted":false,"reason":"threshold","willRetry":false}\n'
printf '%s\n' "$terminal_event"
printf '{"type":"agent_end","messages":[]}\n'

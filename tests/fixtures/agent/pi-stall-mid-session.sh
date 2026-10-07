#!/usr/bin/env bash
set -euo pipefail

IFS= read -r probe_line
if [[ "$probe_line" != *'"type":"get_state"'* ]]; then
    printf 'pi-stall-mid-session: expected get_state command\n' >&2
    exit 2
fi
probe_id="$(sed -n 's/.*"id":"\([^"]*\)".*/\1/p' <<<"$probe_line")"
if [[ -z "$probe_id" ]]; then
    printf 'pi-stall-mid-session: get_state command missing id\n' >&2
    exit 2
fi
printf '%s\n' "{\"type\":\"response\",\"id\":\"${probe_id}\",\"command\":\"get_state\",\"success\":true,\"data\":{\"model\":null,\"thinkingLevel\":\"medium\",\"isStreaming\":false,\"isCompacting\":false,\"messageCount\":0,\"pendingMessageCount\":0}}"

IFS= read -r prompt_line
if [[ "$prompt_line" != *'"type":"prompt"'* ]]; then
    printf 'pi-stall-mid-session: expected prompt command\n' >&2
    exit 2
fi
printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","text":"ack"}}'
printf 'ready\n' >"${LOOM_TEST_STALL_READY:?missing stall readiness receipt}"

# Stay silent while the driver owns stdin; EOF cleans up when the test kills that driver.
while IFS= read -r _; do
    :
done

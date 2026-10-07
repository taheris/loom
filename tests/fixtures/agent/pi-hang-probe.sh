#!/usr/bin/env bash
set -euo pipefail

IFS= read -r probe_line
if [[ "$probe_line" != *'"type":"get_state"'* ]]; then
    printf 'pi-hang-probe: expected get_state command\n' >&2
    exit 2
fi

# Leave the probe unanswered until the driver closes its owned stdin.
while IFS= read -r _; do
    :
done

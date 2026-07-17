#!/usr/bin/env bash
set -euo pipefail

IFS= read -r probe_line
if [[ "$probe_line" != *'"type":"get_state"'* ]]; then
    printf 'pi-hang-probe: expected get_state command\n' >&2
    exit 2
fi

exec sleep 3600

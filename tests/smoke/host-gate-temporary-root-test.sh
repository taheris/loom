#!/usr/bin/env bash
set -euo pipefail

host_gate="${1:?host gate fixture script is required}"
wrix_bin="${2:?absolute Wrix launcher path is required}"
pre_push_checks="${3:?pre-push wrapper path is required}"
fixture=$(mktemp -d "$TMPDIR/host-gate-temp-root.XXXXXXXX")
trap 'rm -rf -- "$fixture"' EXIT
mkdir -m 0700 "$fixture/bin"
export HOST_GATE_REAL_MKTEMP
HOST_GATE_REAL_MKTEMP=$(command -v mktemp)
export HOST_GATE_FORBIDDEN_ROOT
HOST_GATE_FORBIDDEN_ROOT=$(cd /tmp && pwd -P)
printf '#!%s\n' "$(command -v bash)" > "$fixture/bin/mktemp"
cat >> "$fixture/bin/mktemp" <<'SH'
set -euo pipefail
for arg in "$@"; do
    if [[ "$arg" == "$HOST_GATE_FORBIDDEN_ROOT/"* && "$arg" != "$TMPDIR/"* ]]; then
        printf 'fixture attempted allocation under untrusted shared temporary root: %s\n' "$arg" >&2
        exit 78
    fi
done
exec "$HOST_GATE_REAL_MKTEMP" "$@"
SH
chmod u+x "$fixture/bin/mktemp"
PATH="$fixture/bin:$PATH" bash "$host_gate" "$wrix_bin" "$pre_push_checks"

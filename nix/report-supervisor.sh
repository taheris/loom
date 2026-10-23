#!/usr/bin/env bash
set -euo pipefail
: "${out:?native report output is required}"
if [[ -n "${LOOM_NATIVE_REPORT:-}" ]]; then
  read -r code < "$LOOM_NATIVE_REPORT/loom-report/status"
  printf '%s\n' "$code" > "$out"
  exit "$code"
fi
mkdir -p "$out/loom-report"
if "$@" > "$out/loom-report/stdout" 2> "$out/loom-report/stderr"; then
  code=0
else
  code=$?
fi
printf '%s\n' "$code" > "$out/loom-report/status"

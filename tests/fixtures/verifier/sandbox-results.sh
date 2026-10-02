#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$@" > received-targets
result=0
for target in "$@"; do
  cat "${target}.json"
  case "$(<"${target}.exit")" in
    0) ;;
    77) if [[ "$result" == 0 ]]; then result=77; fi ;;
    *) result=1 ;;
  esac
done
if [[ -f producer-exit ]]; then
  result="$(<producer-exit)"
fi
exit "$result"

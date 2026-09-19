# shellcheck shell=bash
set -euo pipefail

args=()
filename=false
for arg in "$@"; do
  if [[ "$filename" == true ]]; then
    case "$arg" in
      wrix/signing-key/repo-key-signing) arg="${WRIX_SIGNING_KEY:?}" ;;
      wrix/allowed_signers) arg=$(git rev-parse --git-path "$arg") ;;
    esac
    filename=false
  elif [[ "$arg" == -f ]]; then
    filename=true
  fi
  args+=("$arg")
done
exec ssh-keygen "${args[@]}"

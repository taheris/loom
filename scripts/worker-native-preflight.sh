#!/usr/bin/env bash
set -euo pipefail

unavailable() {
  local capability="$1"
  local reason="$2"
  printf 'loom-native-preflight: capability unavailable: %s: %s\n' "$capability" "$reason" >&2
  return 78
}

require_tool() {
  local tool="$1"
  if ! command -v "$tool" >/dev/null; then
    unavailable "$tool" 'the provisioned profile does not contain the required executable'
  fi
}

require_nix_state() {
  local root="$1"
  local ancestor
  local actual
  for ancestor in "$root" "$root/var" "$root/var/nix"; do
    actual=$(stat -c '%u:%a' "$ancestor")
    if [[ "$actual" != 0:755 ]]; then
      unavailable nix-state "$ancestor expected 0:755, observed $actual; preserve state and select a conforming profile"
      return 78
    fi
  done
}

probe_tools() (
  local tool
  local work
  for tool in cargo rustc nix timeout jq git; do
    require_tool "$tool"
  done
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  cd "$work"
  git init -q
  cargo --version >/dev/null
  rustc --version >/dev/null
  if ! cargo clippy --version >/dev/null; then
    unavailable clippy 'the provisioned Cargo cannot execute Clippy'
    return 78
  fi
  if ! cargo nextest --version >/dev/null; then
    unavailable nextest 'the provisioned Cargo cannot execute Nextest'
    return 78
  fi
  nix --version >/dev/null
  if ! nix config show --json | jq -e '."build-users-group".value == "" and ."require-sigs".value == true' >/dev/null; then
    unavailable nix-store 'the provisioned store does not retain single-user signed-input policy'
    return 78
  fi
  if ! nix store info --json | jq -e '.url == "local"' >/dev/null; then
    unavailable nix-store 'the provisioned profile cannot open its private local store'
    return 78
  fi
  if [[ "$(nix eval --offline --raw --expr '"native-preflight-eval-ok"')" != native-preflight-eval-ok ]]; then
    unavailable nix-eval 'offline native evaluation failed'
    return 78
  fi
  printf '[package]\nname="loom-executor-probe"\nversion="0.0.0"\nedition="2024"\n' > Cargo.toml
  mkdir src
  printf '#[test]\nfn executor_runs_native_test() { assert!(std::path::Path::new("Cargo.toml").is_file()); }\n' > src/lib.rs
  timeout --kill-after=5s 180s cargo metadata --offline --no-deps --format-version=1 >/dev/null
  timeout --kill-after=5s 30s cargo build --offline
  timeout --kill-after=5s 30s cargo clippy --offline -- -D warnings
  timeout --kill-after=5s 30s cargo clippy --offline --all-targets -- -D warnings
  timeout --kill-after=5s 30s cargo nextest run --offline
)

main() {
  local configuration="${1:-}"
  local version="${2:-}"
  local checksum="${3:-}"
  local helper="${4:?immutable patch initializer is required}"
  local status
  require_tool nix
  require_tool timeout
  require_nix_state /nix
  if [[ -S /nix/var/nix/daemon-socket/socket || -n "${NIX_REMOTE:-}" || -n "${NIX_DAEMON_SOCKET_PATH:-}" ]]; then
    unavailable nix-store 'the worker does not own a private single-user store'
    return 78
  fi
  if [[ -e /homeless-shelter ]]; then
    unavailable purity-guard 'the guard is occupied; preserve it and select a conforming executor'
    return 78
  fi
  probe_tools
  if [[ -f .cargo/config.toml && -f scripts/configure-cargo-patches.sh ]]; then
    if [[ -n "$configuration" ]]; then
      if timeout --kill-after=5s 180s bash "$helper" "$configuration" "$version" "$checksum"; then
        status=0
      else
        status=$?
      fi
    else
      if timeout --kill-after=5s 180s bash "$helper"; then
        status=0
      else
        status=$?
      fi
    fi
    case "$status" in
      0) ;;
      124|137) return "$status" ;;
      *) printf 'loom-native-preflight: project patch initialization unavailable (exit %s); source repair remains enabled, required verification remains pending\n' "$status" >&2 ;;
    esac
  fi
  if timeout --kill-after=5s 180s cargo metadata --offline --no-deps --format-version=1 >/dev/null; then
    :
  else
    status=$?
    case "$status" in
      124|137) return "$status" ;;
      *) printf 'loom-native-preflight: project metadata unavailable; source repair remains enabled (exit %s), required verification remains pending\n' "$status" >&2 ;;
    esac
  fi
  printf 'loom-native-preflight: source-feedback tools available; required verification remains pending\n' >&2
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi

#!/usr/bin/env bash
set -euo pipefail

trusted_root() {
  local path="$1" metadata owner mode checked=0
  local -a ancestors=()

  # The filesystem root anchors traversal; validate every canonical child.
  while [[ "$path" != / ]]; do
    ancestors+=("$path")
    path="${path%/*}"
    [[ -n "$path" ]] || path=/
  done
  (("${#ancestors[@]}" > 0)) || return 0
  metadata=$(stat --format='%u %a' -- "${ancestors[@]}") || return 1
  while read -r owner mode; do
    [[ "$owner" =~ ^[0-9]+$ && "$mode" =~ ^[0-7]+$ ]] || return 1
    if [[ "$owner" != "$EUID" && "$owner" != 0 ]] ||
      (( (8#$mode & 0022) != 0 && (8#$mode & 01000) == 0 )); then
      return 1
    fi
    ((checked += 1))
  done <<< "$metadata"
  ((checked == ${#ancestors[@]}))
}

private_root() {
  local candidate root

  for candidate in "${NIX_BUILD_TOP:-}" "${TMPDIR:-}" /tmp; do
    if [[ -z "$candidate" || ! -d "$candidate" ]]; then
      continue
    fi
    root=$(realpath -e -- "$candidate")
    if trusted_root "$root"; then
      # Nested AF_UNIX sockets must also fit Darwin's sockaddr_un.
      mktemp -d "$root/lf.XXXXXXXX"
      return
    fi
    printf 'Rejecting unsafe temporary root: %s\n' "$root" >&2
  done
  printf 'No trusted temporary root is available for test fixtures\n' >&2
  return 1
}

fixture_process=true
if [[ "${1:-}" == --build-campaign ]]; then
  fixture_process=false
  shift
elif [[ "${1:-}" == --test-process ]]; then
  shift
fi

if [[ "$#" == 0 ]]; then
  printf 'Usage: private-fixture-tests.sh [--build-campaign|--test-process] <command> [args...]\n' >&2
  exit 2
fi

# Retain the admitted Nix confinement before replacing homes and private store state.
nix_sandbox=
if [[ "$fixture_process" == false && -n "${LOOM_TEST_NIX_REGISTRATION:-}" ]]; then
  nix_sandbox=$("${LOOM_NATIVE_NIX_BIN:?missing declared Nix tool}" --extra-experimental-features nix-command config show sandbox)
  case "$nix_sandbox" in
    true|false) ;;
    *) printf 'Unsupported admitted Nix sandbox policy: %s\n' "$nix_sandbox" >&2; exit 1 ;;
  esac
fi

# Shared compiler daemons must not capture a disposable fixture TMPDIR.
for name in "${!SCCACHE_@}"; do
  unset "$name"
done
export RUSTC_WRAPPER="" RUSTC_WORKSPACE_WRAPPER=""
export CARGO_BUILD_RUSTC_WRAPPER="" CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=""

# Packaged campaigns provide a resource-aware scheduler. Direct Cargo runs
# retain Nextest's native scheduling unless the caller explicitly supplies one.
if [[ "$fixture_process" == false && -z "${NEXTEST_TEST_THREADS:-}" && -n "${LOOM_TEST_SCHEDULING:-}" ]]; then
  NEXTEST_TEST_THREADS=$(bash "$LOOM_TEST_SCHEDULING")
  export NEXTEST_TEST_THREADS
fi

if [[ -n "${LOOM_TEST_SOURCE_ROOT:-}" ]]; then
  export INSTA_WORKSPACE_ROOT="$LOOM_TEST_SOURCE_ROOT"
fi

umask 077
# Isolate homes, not the registry data that compiled the running tests.
if [[ -z "${CARGO_HOME:-}" && -n "${HOME:-}" && "$HOME" != /homeless-shelter ]]; then
  export CARGO_HOME="$HOME/.cargo"
fi
TMPDIR=$(private_root)
export TMPDIR
# Fixture tools must never create Nix\'s forbidden default home on the host.
export HOME="$TMPDIR/home"
mkdir -m 700 -- "$HOME"
# Runtime fixtures cannot inherit targets; primary compilation preserves its admitted target.
if [[ "$fixture_process" == true ]]; then
  if [[ -n "${LOOM_TEST_RUNTIME_PATH:-}" ]]; then
    export PATH="$PATH:$LOOM_TEST_RUNTIME_PATH"
  fi
  export CARGO_TARGET_DIR="$TMPDIR/cargo-target"
  export CARGO_BUILD_TARGET_DIR="$CARGO_TARGET_DIR"
  export CARGO_BUILD_BUILD_DIR="$CARGO_TARGET_DIR"
  # The outer campaign supplies concurrency; individual Go fixtures need one worker.
  export GOMAXPROCS="${GOMAXPROCS:-1}"
  # SDK inputs are already admitted; private fixture outputs are not public cache entries.
  export NIX_CONFIG="${NIX_CONFIG:-}"$'\nsubstitute = false'
fi
# Empty values disable Cargo-configured wrappers; unsetting would re-enable them.
export RUSTC_WRAPPER="" CARGO_BUILD_RUSTC_WRAPPER=""
export RUSTC_WORKSPACE_WRAPPER="" CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=""
for key in "${!SCCACHE_@}"; do
  unset "$key"
done
export SCCACHE_DIR="$TMPDIR/sccache"
export SCCACHE_SERVER_UDS="$TMPDIR/sccache.sock"
# Reconciliation locks must be as private and disposable as fixture worktrees.
# In unsandboxed Nix checks, the default HOME is the forbidden /homeless-shelter.
export XDG_STATE_HOME="$TMPDIR/state"
export LOOM_TEST_STORAGE=1
mkdir -p -- "$XDG_STATE_HOME/loom/trusted-cargo"
# Tiny fixture checkouts reserve one MiB each; real allocated growth is still charged.
printf '{"reservation_bytes":1048576}\n' > "$XDG_STATE_HOME/loom/trusted-cargo/policy.json"
# Seed only declared readonly inputs into the packaged suite's disposable local store.
if [[ "$fixture_process" == false && -n "${LOOM_TEST_NIX_REGISTRATION:-}" ]]; then
  export NIX_REMOTE="local://?real=$TMPDIR/store" NIX_STORE_DIR=/nix/store
  export NIX_STATE_DIR="$TMPDIR/nix-state" NIX_LOG_DIR="$TMPDIR/nix-log"
  export NIX_CONF_DIR="$TMPDIR/nix-conf"
  mkdir -p "$NIX_STATE_DIR" "$NIX_LOG_DIR" "$NIX_CONF_DIR"
  printf '%s\n' \
    'experimental-features = nix-command flakes' \
    'build-users-group =' \
    "store = $NIX_REMOTE" \
    'require-sigs = true' \
    "sandbox = $nix_sandbox" \
    'sandbox-fallback = false' \
    'allow-import-from-derivation = false' \
    'substituters =' \
    'builders =' > "$NIX_CONF_DIR/nix.conf"
  # Ambient settings must not override this owned store or its admitted policy.
  NIX_CONFIG="${NIX_CONFIG:-}"$'\n'"$(< "$NIX_CONF_DIR/nix.conf")"
  export NIX_CONFIG
  # Runtime tools stay off the compiler PATH, including during fixture seeding.
  "${LOOM_NATIVE_NIX_BIN:?missing declared Nix tool}"-store --load-db < "$LOOM_TEST_NIX_REGISTRATION"
  # Keep canonical logical identities, but own all physical bytes and store writes.
  # A strict Darwin builder cannot initialize the host store's .links directory.
  "$LOOM_NATIVE_NIX_BIN" path-info --all > "$TMPDIR/sdk-paths"
  while IFS= read -r path; do
    cp -a --no-preserve=ownership --reflink=auto -- "$path" "$TMPDIR/store/${path##*/}"
  done < "$TMPDIR/sdk-paths"
  "$LOOM_NATIVE_NIX_BIN"-store --verify --check-contents
fi

writable_directories() {
  local root="$1"
  if [[ -L "$root" || ! -d "$root" || ! -O "$root" ]]; then
    return 0
  fi
  # Prune foreign ownership and never follow links, including at chmod time.
  find -P "$root" ! -user "$EUID" -prune -o -type d -exec chmod -h u+w -- {} +
}

cleanup() {
  local rc=$?
  if [[ "$rc" == 0 ]]; then
    # Nix copies are readonly; restore only owned directories without following symlinks.
    writable_directories "$TMPDIR"
    rm -rf -- "$TMPDIR"
  else
    printf 'Failed fixture root retained at %s\n' "$TMPDIR" >&2
  fi
}
trap cleanup EXIT
"$@"

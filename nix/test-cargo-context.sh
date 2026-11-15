# shellcheck shell=bash
# Embedded by the shared test-app constructor, before any Cargo invocation.
# Keep one writable target per checkout and immutable toolchain, not per run.
: "${LOOM_TEST_TOOLCHAIN:?the test app must declare its immutable toolchain}"
umask 077
export RUSTC="$LOOM_TEST_TOOLCHAIN/bin/rustc"
export RUSTDOC="$LOOM_TEST_TOOLCHAIN/bin/rustdoc"
export CARGO_BUILD_RUSTC="$RUSTC" CARGO_BUILD_RUSTDOC="$RUSTDOC"
unset RUSTUP_TOOLCHAIN
if [[ -z "${NEXTEST_TEST_THREADS:-}" ]]; then
  : "${LOOM_TEST_SCHEDULING:?the test app must declare its scheduling helper}"
  NEXTEST_TEST_THREADS=$("$BASH" "$LOOM_TEST_SCHEDULING")
  export NEXTEST_TEST_THREADS
fi
printf 'Native test scheduling: %s threads\n' "$NEXTEST_TEST_THREADS" >&2
# Test campaigns need useful backtraces, not full local-variable DWARF or an
# incremental database for every test binary. Assertions/optimisation stay dev/test.
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_INCREMENTAL=false CARGO_PROFILE_TEST_INCREMENTAL=false
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only

expected_cargo=$(readlink -f "$LOOM_TEST_TOOLCHAIN/bin/cargo")
selected_cargo=$(readlink -f "$(command -v cargo)")
if [[ "$selected_cargo" != "$expected_cargo" ]]; then
  printf 'Test app selected a foreign Cargo executable; refusing mixed toolchains.\n' >&2
  exit 78
fi
compiler_version=$("$RUSTC" -vV)
cargo_version=$("$LOOM_TEST_TOOLCHAIN/bin/cargo" -Vv)
# Cargo fingerprints retain the profile/flag inputs. Do not allocate another
# writable target just because sources or the footprint settings change.
context=$(printf '%s\n%s\n%s\n' "$LOOM_TEST_TOOLCHAIN" "$compiler_version" "$cargo_version" | sha256sum)
context=${context%% *}
test_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)
if [[ -L "$test_root/target" || -L "$test_root/target/acceptance-$context" ]]; then
  printf 'Test target is indirect; refusing to use it.\n' >&2
  exit 78
fi
if [[ "$(uname -s)" == Linux ]]; then
  # Source checkouts can be FUSE/virtiofs. Put executing test images on native
  # storage so Linux's existing immutable-running-image guard can safely share
  # their content identity. This is reusable output, never cached test evidence.
  cache="${XDG_CACHE_HOME:-${HOME:?test cache home required}/.cache}/loom/test-targets"
  if [[ "$cache" != /* ]]; then
    printf 'Test cache must be absolute.\n' >&2
    exit 78
  fi
  workspace_identity=$(printf '%s\n%s\n' "$(realpath -e -- "$test_root")" "$(stat --format='%d:%i' -- "$test_root")" | sha256sum)
  workspace_identity=${workspace_identity%% *}
  export CARGO_TARGET_DIR="$cache/$workspace_identity/acceptance-$context"
  candidate="$CARGO_TARGET_DIR"
  while [[ "$candidate" != / ]]; do
    if [[ -L "$candidate" ]]; then
      printf 'Test cache is indirect: %s\n' "$candidate" >&2
      exit 78
    fi
    if [[ -e "$candidate" ]]; then
      if [[ ! -d "$candidate" ]]; then
        printf 'Test cache ancestor is not a directory: %s\n' "$candidate" >&2
        exit 78
      fi
      read -r owner mode <<< "$(stat --format='%u %a' -- "$candidate")"
      if [[ "$owner" != "$EUID" && "$owner" != 0 ]] ||
        (( (8#$mode & 0022) != 0 && (8#$mode & 01000) == 0 )); then
        printf 'Test cache ancestor is unsafe: %s\n' "$candidate" >&2
        exit 78
      fi
    fi
    candidate=$(dirname -- "$candidate")
  done
else
  export CARGO_TARGET_DIR="$test_root/target/acceptance-$context"
fi
mkdir -p "$CARGO_TARGET_DIR"
printf 'Test compiler: %s\nTest Cargo: %s\nReusable test target: %s\n' \
  "${compiler_version%%$'\n'*}" "${cargo_version%%$'\n'*}" "$CARGO_TARGET_DIR" >&2

# shellcheck shell=bash
# Embedded by the shared test-app constructor, before any Cargo invocation.
# Keep one writable target per checkout and immutable toolchain, not per run.
: "${LOOM_TEST_TOOLCHAIN:?the test app must declare its immutable toolchain}"
umask 077
export RUSTC="$LOOM_TEST_TOOLCHAIN/bin/rustc"
export RUSTDOC="$LOOM_TEST_TOOLCHAIN/bin/rustdoc"
export CARGO_BUILD_RUSTC="$RUSTC" CARGO_BUILD_RUSTDOC="$RUSTDOC"
unset RUSTUP_TOOLCHAIN
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
export CARGO_TARGET_DIR="$test_root/target/acceptance-$context"
mkdir -p "$CARGO_TARGET_DIR"
printf 'Test compiler: %s\nTest Cargo: %s\nReusable test target: %s\n' \
  "${compiler_version%%$'\n'*}" "${cargo_version%%$'\n'*}" "$CARGO_TARGET_DIR" >&2

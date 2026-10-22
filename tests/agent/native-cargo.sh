#!/usr/bin/env bash
set -euo pipefail

cargo_bin="${1:?Cargo wrapper path is required}"
config="${2:?Cargo patch config is required}"
version="${3:?Cargo patch version is required}"
checksum="${4:?Cargo patch checksum is required}"
prek_bin="${5:?Pinned hook runner is required}"
repo_root=$(git rev-parse --show-toplevel)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
real_nix=$(command -v nix)
export HOME="$fixture/home"
mkdir -p "$HOME"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export NIX_CONF_DIR="$fixture/nix-conf" NIX_USER_CONF_FILES=/dev/null
export NIX_CONFIG='experimental-features ='
unset GIT_CONFIG_COUNT
mkdir -p "$fixture/repo/scripts" "$fixture/repo/.cargo" "$fixture/bin" "$NIX_CONF_DIR"
if "$real_nix" eval --raw --expr '"unexpected"' > "$fixture/nix-features.log" 2>&1; then
    printf 'Bootstrap fixture unexpectedly enables nix-command\n' >&2
    exit 1
fi
[[ "$(<"$fixture/nix-features.log")" == *"experimental Nix feature 'nix-command' is disabled"* ]]
export NIX_REMOTE="local?root=$fixture/unavailable-store"
touch "$fixture/unavailable-store"
if "$real_nix" --extra-experimental-features nix-command eval --raw --expr '"unexpected"' > "$fixture/nix-store.log" 2>&1; then
    printf 'Bootstrap fixture unexpectedly opens an unavailable store\n' >&2
    exit 1
fi
[[ "$(<"$fixture/nix-store.log")" == *'Not a directory'* ]]
cp "$repo_root/scripts/configure-cargo-patches.sh" "$repo_root/scripts/cargo-patch-key.nix" "$fixture/repo/scripts/"
cd "$fixture/repo"
git init -q
printf 'include = [{ path = "nix-patches.toml", optional = true }]\n' > .cargo/config.toml
cat > Cargo.lock <<TOML
version = 4
[[package]]
name = "nextest-filtering"
version = "$version"
checksum = "$checksum"
TOML
cp Cargo.lock "$fixture/original-Cargo.lock"
for probe in --version -V -vV -Vv --help -h version help; do
    [[ "$("$cargo_bin" "$probe")" == "$probe" ]]
    [[ ! -e .cargo/nix-patches.toml ]]
done
[[ "$("$cargo_bin" build --locked)" == $'build\n--locked' ]]
[[ "$(readlink .cargo/nix-patches.toml)" == "$config" ]]
cmp Cargo.lock "$fixture/original-Cargo.lock"

export REAL_NIX="$real_nix"
export NIX_PATCH_BUILD_RESULT="$fixture/refreshed.toml"
export NIX_PATCH_BUILD_CALLS="$fixture/build-calls"
cp "$config" "$NIX_PATCH_BUILD_RESULT"
printf '#!%s\n' "$(command -v bash)" > "$fixture/bin/nix"
cat >> "$fixture/bin/nix" <<'SH'
set -euo pipefail
if [[ "${1:-}" == --extra-experimental-features && "${3:-}" == build ]]; then
    [[ "$2" == 'nix-command flakes' ]]
    if [[ "${NIX_PATCH_BUILD_FAIL:-}" == 1 ]]; then
        exit 78
    fi
    printf '%s\n' "$@" >> "$NIX_PATCH_BUILD_CALLS"
    printf '%s\n' "$NIX_PATCH_BUILD_RESULT"
else
    exec "$REAL_NIX" "$@"
fi
SH
chmod +x "$fixture/bin/nix"
export PATH="$fixture/bin:$PATH"
cp "$repo_root/.pre-commit-config.yaml" .pre-commit-config.yaml
git add scripts .pre-commit-config.yaml .cargo/config.toml Cargo.lock
rm .cargo/nix-patches.toml
"$prek_bin" run native-cargo-capabilities --all-files --hook-stage pre-commit
[[ "$(readlink .cargo/nix-patches.toml)" == "$NIX_PATCH_BUILD_RESULT" ]]
[[ -s "$NIX_PATCH_BUILD_CALLS" ]]
rm "$NIX_PATCH_BUILD_CALLS"
grep -v '^LOOM_ISSUES_BD_BIN = ' "$config" > "$fixture/legacy-issues-config.toml"
ln -sf "$fixture/legacy-issues-config.toml" .cargo/nix-patches.toml
"$prek_bin" run native-cargo-capabilities --all-files --hook-stage pre-commit
[[ "$(readlink .cargo/nix-patches.toml)" == "$NIX_PATCH_BUILD_RESULT" ]]
[[ -s "$NIX_PATCH_BUILD_CALLS" ]]
if [[ $(uname -s) == Linux ]]; then
    for capability in LOOM_NATIVE_FLOCK_BIN LOOM_NATIVE_SHELL_BIN LOOM_NATIVE_STAT_BIN; do
        rm "$NIX_PATCH_BUILD_CALLS"
        grep -v "^$capability = " "$config" > "$fixture/legacy-entry-config.toml"
        bash scripts/configure-cargo-patches.sh "$fixture/legacy-entry-config.toml" "$version" "$checksum"
        [[ "$(readlink .cargo/nix-patches.toml)" == "$NIX_PATCH_BUILD_RESULT" ]]
        [[ -s "$NIX_PATCH_BUILD_CALLS" ]]
    done
fi
rm "$NIX_PATCH_BUILD_CALLS"
export NIX_PATCH_BUILD_FAIL=1
if "$prek_bin" run native-cargo-capabilities --all-files --hook-stage pre-commit > "$fixture/hook-failure.log" 2>&1; then
    printf 'Cargo capability hook hid initialization failure\n' >&2
    exit 1
fi
[[ "$(readlink .cargo/nix-patches.toml)" == "$NIX_PATCH_BUILD_RESULT" ]]
unset NIX_PATCH_BUILD_FAIL
cat > Cargo.lock <<'TOML'
version = 4
[[package]]
name = "nextest-filtering"
version = "999.0.0"
checksum = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
TOML
cp Cargo.lock "$fixture/changed-Cargo.lock"
[[ "$("$cargo_bin" build --locked)" == $'build\n--locked' ]]
[[ "$(readlink .cargo/nix-patches.toml)" == "$NIX_PATCH_BUILD_RESULT" ]]
[[ -s "$NIX_PATCH_BUILD_CALLS" ]]
cmp Cargo.lock "$fixture/changed-Cargo.lock"

cat >> Cargo.lock <<'TOML'
[[package]]
name = "nextest-filtering"
version = "999.0.1"
checksum = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
TOML
if "$cargo_bin" build --locked > "$fixture/ambiguous.log" 2>&1; then
    printf 'Cargo wrapper accepted an ambiguous patch identity\n' >&2
    exit 1
fi
[[ "$(<"$fixture/ambiguous.log")" == *'Cargo.lock must contain exactly one nextest-filtering package'* ]]
[[ "$(readlink .cargo/nix-patches.toml)" == "$NIX_PATCH_BUILD_RESULT" ]]

printf 'paths = []\n' > "$fixture/legacy-config.toml"
cp "$NIX_PATCH_BUILD_CALLS" "$fixture/build-calls-before-legacy"
if bash scripts/configure-cargo-patches.sh "$fixture/legacy-config.toml" "$version" "$checksum" > "$fixture/legacy-ambiguous.log" 2>&1; then
    printf 'Legacy Cargo configuration bypassed ambiguous patch identity\n' >&2
    exit 1
fi
[[ "$(<"$fixture/legacy-ambiguous.log")" == *'Cargo.lock must contain exactly one nextest-filtering package'* ]]
cmp "$NIX_PATCH_BUILD_CALLS" "$fixture/build-calls-before-legacy"

cd "$fixture"
[[ "$("$cargo_bin" build --locked)" == $'build\n--locked' ]]
[[ ! -e .cargo/nix-patches.toml ]]

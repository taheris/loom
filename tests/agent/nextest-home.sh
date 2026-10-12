#!/usr/bin/env bash
set -euo pipefail

hook="${1:?cargo-nextest preUnpack hook is required}"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/source/src" "$fixture/build dir"
cd "$fixture/source"
cat > Cargo.toml <<'TOML'
[package]
name = "cargo-home-probe"
version = "0.0.0"
edition = "2024"
TOML
cat > src/lib.rs <<'RS'
#[test]
fn cargo_subprocess_inherits_private_homes() {
    let build_dir = std::env::var("TMPDIR").unwrap();
    for variable in ["HOME", "CARGO_HOME"] {
        let home = std::env::var(variable).unwrap();
        assert!(std::path::Path::new(&home).starts_with(&build_dir));
    }
}
RS

export HOME="$fixture/unisolated-home"
export CARGO_TARGET_DIR="$fixture/control-target"
unset CARGO_HOME
cargo build --offline
[[ -f "$HOME/.cargo/.global-cache" ]]

export HOME="$fixture/forbidden-home"
export CARGO_HOME="$fixture/forbidden-cargo-home"
export TMPDIR="$fixture/build dir"
export CARGO_TARGET_DIR="$TMPDIR/target"
# shellcheck source=/dev/null
source "$hook"
[[ "$HOME" == "$TMPDIR/"* ]]
[[ "$CARGO_HOME" == "$TMPDIR/"* ]]
[[ ! -e "$HOME/.cargo/.global-cache" && ! -e "$CARGO_HOME/.global-cache" ]]
cargo build --offline --locked
cargo test --offline --locked
[[ -f "$CARGO_HOME/.global-cache" ]]
[[ ! -e "$fixture/forbidden-home" && ! -e "$fixture/forbidden-cargo-home" ]]

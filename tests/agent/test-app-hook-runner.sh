#!/usr/bin/env bash
set -euo pipefail

required_app=${1:?required-suite entry is required}
full_app=${2:?full-suite entry is required}
runner=${3:?immutable hook runner is required}
beads=${4:?immutable Beads executable is required}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export HOME="$work/home" CARGO_HOME="$work/cargo-home"
mkdir -p "$HOME" "$CARGO_HOME"

# Only unrelated Nix/system stages are fixture tools. Cargo, Clippy and
# Nextest are real; the shared production app constructor supplies the pin.
for app in "$required_app" "$full_app"; do
  for ambient in absent conflicting; do
    fixture=$(mktemp -d "$work/project.XXXXXXXX")
    mkdir -p "$fixture/src" "$fixture/.cargo"
    git -C "$fixture" init -q
    printf '[package]\nname="hook-runner-fixture"\nversion="0.0.0"\nedition="2024"\n' > "$fixture/Cargo.toml"
    printf 'include = [{ path = "missing.toml", optional = true }]\n' > "$fixture/.cargo/config.toml"
    printf '%s' "$runner" > "$fixture/expected-runner"
    printf '%s' "$beads" > "$fixture/expected-beads"
    printf 'fn main() {}\n#[test] fn immutable_capabilities_are_embedded() { assert_eq!(option_env!("LOOM_PREK_BIN"), Some(include_str!("../expected-runner"))); assert_eq!(option_env!("LOOM_ISSUES_BD_BIN"), Some(include_str!("../expected-beads"))); }\n' > "$fixture/src/main.rs"
    if [[ "$ambient" == absent ]]; then
      unset LOOM_PREK_BIN LOOM_ISSUES_BD_BIN
    else
      export LOOM_PREK_BIN="$fixture/untrusted-ambient-runner"
      export LOOM_ISSUES_BD_BIN="$fixture/untrusted-ambient-beads"
    fi
    if ! (cd "$fixture"; timeout --kill-after=5s 180s "$app") > "$fixture/suite.log" 2>&1; then
      cat "$fixture/suite.log"
      exit 1
    fi
    cat "$fixture/suite.log"
    grep -F '1 passed' "$fixture/suite.log"
    [[ ! -e "$fixture/.cargo/missing.toml" ]]
    printf 'test-app-hook-runner-ok app=%s ambient=%s\n' "$app" "$ambient"
  done
done

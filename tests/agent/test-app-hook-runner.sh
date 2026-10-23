#!/usr/bin/env bash
set -euo pipefail

required_app=${1:?required-suite entry is required}
full_app=${2:?full-suite entry is required}
runner=${3:?immutable hook runner is required}
beads=${4:?immutable Beads executable is required}
resource_preflight=${5:?actual native source preflight is required}
cache=${6:?immutable compiler cache is required}
nix_bin=${7:?immutable Nix executable is required}
timeout_bin=$(command -v timeout)
work=$(mktemp -d)
cleanup() {
  local rc=$?
  if [[ -n "${SCCACHE_SERVER_UDS:-}" ]]; then
    "$cache" --stop-server >/dev/null 2>&1 || true
  fi
  if [[ "$rc" == 0 ]]; then
    rm -rf "$work"
  else
    printf 'failed native toolchain fixture retained at %s\n' "$work" >&2
  fi
}
trap cleanup EXIT
export HOME="$work/home" CARGO_HOME="$work/cargo-home"
export SCCACHE_DIR="$work/cache" SCCACHE_SERVER_UDS="$work/cache.sock"
export CC='sccache gcc' CXX='sccache g++'
# Verify provisioned compiler wrappers without asserting cache/backend behavior.
export RUSTC_WRAPPER="$cache" CARGO_INCREMENTAL=0
mkdir -p "$HOME" "$CARGO_HOME" "$work/empty-bin"

# Unrelated Nix stages are fixture tools. Cargo, Clippy, Nextest,
# C/C++/assembly and sccache are real. An empty caller PATH ensures the shared
# production app constructor, not the parent build shell, supplies its tools.
for app in "$required_app" "$full_app"; do
  for ambient in absent conflicting; do
    fixture=$(mktemp -d "$work/project.XXXXXXXX")
    mkdir -p "$fixture/src" "$fixture/.cargo" "$fixture/tests/verify"
    cp "$resource_preflight" "$fixture/tests/verify/native-resources.py"
    printf '{ outputs = { self }: {}; }\n' > "$fixture/flake.nix"
    git -C "$fixture" init -q
    printf '[package]\nname="hook-runner-fixture"\nversion="0.0.0"\nedition="2024"\n' > "$fixture/Cargo.toml"
    printf 'include = [{ path = "missing.toml", optional = true }]\n' > "$fixture/.cargo/config.toml"
    printf '%s' "$runner" > "$fixture/expected-runner"
    printf '%s' "$beads" > "$fixture/expected-beads"
    printf '%s' "$nix_bin" > "$fixture/expected-nix"
    printf 'fn main() {}\n#[test] fn immutable_capabilities_are_embedded() { assert!(std::hint::black_box(cfg!(debug_assertions))); assert_eq!(option_env!("LOOM_PREK_BIN"), Some(include_str!("../expected-runner"))); assert_eq!(option_env!("LOOM_ISSUES_BD_BIN"), Some(include_str!("../expected-beads"))); assert_eq!(option_env!("LOOM_NATIVE_NIX_BIN"), Some(include_str!("../expected-nix"))); }\n' > "$fixture/src/main.rs"
    printf 'int native_c(void) { return 42; }\n' > "$fixture/native.c"
    printf 'extern "C" int native_cpp() { return 42; }\n' > "$fixture/native.cpp"
    printf '.text\n' > "$fixture/native.S"
    cat > "$fixture/build.rs" <<'RS'
use std::{env, fs, path::PathBuf, process::Command};
fn main() {
    assert_eq!(env::var("CARGO_INCREMENTAL").unwrap(), "0");
    assert_eq!(env::var("CARGO_PROFILE_DEV_DEBUG").unwrap(), "line-tables-only");
    assert_eq!(env::var("CARGO_PROFILE_TEST_DEBUG").unwrap(), "line-tables-only");
    let toolchain = env::var("LOOM_TEST_TOOLCHAIN").unwrap();
    assert_eq!(env::var("RUSTC").unwrap(), format!("{toolchain}/bin/rustc"));
    let target = PathBuf::from(env::var_os("CARGO_TARGET_DIR").unwrap());
    assert!(target.file_name().unwrap().to_str().unwrap().starts_with("acceptance-"));
    assert_eq!(fs::canonicalize(target.parent().unwrap().parent().unwrap()).unwrap(), fs::canonicalize(env::var_os("CARGO_MANIFEST_DIR").unwrap()).unwrap());
    assert_eq!(env::var("CC").unwrap(), "sccache gcc");
    assert_eq!(env::var("CXX").unwrap(), "sccache g++");
    for (compiler, source, name) in [
        ("gcc", "native.c", "c"),
        ("gcc", "native.S", "assembly"),
        ("g++", "native.cpp", "cpp"),
    ] {
        let object = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join(format!("{name}.o"));
        let result = Command::new("sccache")
            .arg(compiler)
            .args(["-c", source, "-o"])
            .arg(&object)
            .output()
            .unwrap();
        assert!(result.status.success(), "{compiler} {source}: {}", String::from_utf8_lossy(&result.stderr));
        assert!(fs::metadata(object).unwrap().len() > 0);
        println!("cargo:rerun-if-changed={source}");
    }
}
RS
    git -C "$fixture" add --all
    if [[ "$ambient" == absent ]]; then
      unset LOOM_PREK_BIN LOOM_ISSUES_BD_BIN LOOM_NATIVE_NIX_BIN RUSTC RUSTDOC CARGO_BUILD_RUSTC CARGO_BUILD_RUSTDOC RUSTUP_TOOLCHAIN CARGO_TARGET_DIR
    else
      export LOOM_PREK_BIN="$fixture/untrusted-ambient-runner"
      export LOOM_ISSUES_BD_BIN="$fixture/untrusted-ambient-beads"
      export LOOM_NATIVE_NIX_BIN="$fixture/untrusted-ambient-nix"
      export RUSTC="$fixture/foreign-rustc" RUSTDOC="$fixture/foreign-rustdoc"
      export CARGO_BUILD_RUSTC="$RUSTC" CARGO_BUILD_RUSTDOC="$RUSTDOC"
      export RUSTUP_TOOLCHAIN=foreign-toolchain CARGO_TARGET_DIR="$fixture/foreign-target"
    fi
    if ! (cd "$fixture"; PATH="$work/empty-bin" "$timeout_bin" --kill-after=5s 180s "$app") > "$fixture/suite.log" 2>&1; then
      cat "$fixture/suite.log"
      exit 1
    fi
    cat "$fixture/suite.log"
    grep -F '1 passed' "$fixture/suite.log"
    [[ ! -e "$fixture/.cargo/missing.toml" ]]
    [[ ! -e "$fixture/foreign-target" && ! -e "$fixture/target/debug" ]]
    printf 'test-app-hook-runner-ok app=%s ambient=%s\n' "$app" "$ambient"
    if (cd "$fixture"; LOOM_FIXTURE_INVALID_METADATA=1 PATH="$work/empty-bin" "$timeout_bin" --kill-after=5s 30s "$app") > "$fixture/rejected.log" 2>&1; then
      printf 'suite accepted malformed native metadata\n' >&2
      exit 1
    fi
    grep -F "KeyError: 'path'" "$fixture/rejected.log"
  done
done

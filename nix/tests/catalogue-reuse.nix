# The real constructor and legacy compiler recipe must enumerate the same tests.
{ pkgs, loom }:
let
  checked = import ../verification.nix;
  fixture = checked.resource {
    root = ../..;
    origin = "tests/fixtures/catalogue-reuse";
  };
  groups = [ "@global" ] ++ builtins.attrNames fixture._nextest.test-groups;
  verificationTool = import ../verification-tool.nix { inherit pkgs; };
  cargo = pkgs.writeShellScript "catalogue-cargo-forwarder" ''
    set -euo pipefail
    printf '%s' "$CATALOGUE_LIST_STAGE" >> "$out/cargo-calls"
    printf ' %s' "$@" >> "$out/cargo-calls"
    printf '\n' >> "$out/cargo-calls"
    exec ${loom.toolchain}/bin/cargo "$@"
  '';
  nextest = pkgs.writeShellScriptBin "cargo-nextest" ''
    set -euo pipefail
    export CATALOGUE_LIST_STAGE=first
    for arg in "$@"; do
      if [[ "$arg" == --binaries-metadata ]]; then export CATALOGUE_LIST_STAGE=reuse; fi
    done
    export CATALOGUE_WRAPPER_LOG="$out/wrappers.log" CARGO=${cargo}
    exec ${pkgs.cargo-nextest}/bin/cargo-nextest "$@"
  '';
  args = {
    src = fixture.path;
    cargoToml = "${fixture.path}/Cargo.toml";
    cargoLock = "${fixture.path}/Cargo.lock";
    cargoVendorDir = pkgs.emptyDirectory;
    cargoArtifacts = null;
    CARGO_PROFILE = "dev";
    nativeBuildInputs = [
      nextest
      pkgs.jq
      pkgs.git
    ];
  };
  candidate =
    (checked.cargo {
      inherit args;
      inherit (loom) craneLib;
      operation = "test";
      runtime = "native-process";
      resources = [ fixture ];
    }).compilation;
  baseline = loom.craneLib.mkCargoDerivation (
    args
    // {
      prePatch = ''export CARGO_TARGET_DIR="$out/target"'';
      dontFixup = true;
      NIX_NO_SELF_RPATH = 1;
      doCheck = false;
      doInstallCargoArtifacts = false;
      buildPhaseCargoCommand = ''
        export HOME=$(mktemp -d)
        export GIT_CONFIG_SYSTEM=/dev/null
        export CARGO_TARGET_DIR="$out/target"
        mkdir -p "$out"
        cargo metadata --offline --locked --manifest-path "$src/Cargo.toml" --format-version 1 > "$out/cargo.json"
        cargo nextest list --offline --locked --manifest-path "$src/Cargo.toml" --workspace --all-targets --cargo-profile "$CARGO_PROFILE" --run-ignored all --ignore-default-filter --list-type binaries-only --message-format json > "$out/binaries.json"
        cargo nextest list --offline --locked --manifest-path "$src/Cargo.toml" --workspace --all-targets --cargo-profile "$CARGO_PROFILE" --run-ignored all --ignore-default-filter --message-format json > "$out/nextest.json"
        cargo nextest list --offline --locked --manifest-path "$src/Cargo.toml" --workspace --all-targets --cargo-profile "$CARGO_PROFILE" --run-ignored all --message-format json > "$out/default.json"
        index=0
        for group in ${pkgs.lib.escapeShellArgs groups}; do
          cargo nextest list --offline --locked --manifest-path "$src/Cargo.toml" --workspace --all-targets --cargo-profile "$CARGO_PROFILE" --run-ignored all --ignore-default-filter -E "group(=$group)" --message-format json > "$out/group-$index.json"
          index=$((index + 1))
        done
        printf '%s' ${pkgs.lib.escapeShellArg (builtins.concatStringsSep "\n" groups)} > "$out/groups.txt"
      '';
      installPhaseCommand = ''mkdir -p "$out"'';
    }
  );
in
checked.check {
  runtime = "native-process";
  resources = [ fixture ] ++ verificationTool.loom.sources;
  evaluation = import ../native-context.nix;
  build =
    _:
    pkgs.runCommandCC "catalogue-reuse-conformance"
      {
        nativeBuildInputs = [
          verificationTool
          pkgs.coreutils
          pkgs.jq
          pkgs.cargo-nextest
          loom.toolchain
          pkgs.bash
        ]
        ++ loom.testCompilerHooks;
        passthru = { inherit candidate baseline; };
      }
      ''
        set -euo pipefail
        export HOME=$(mktemp -d) CATALOGUE_WRAPPER_LOG="$TMPDIR/run-wrappers.log" CATALOGUE_LIST_STAGE=run
        loom-verification catalogue-reuse ${candidate} ${baseline} ${fixture.path} "$out" ${pkgs.writeText "catalogue-owned-script" candidate.buildPhase}
      '';
}

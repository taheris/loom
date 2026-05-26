# tests/loom/default.nix
#
# Builds the two test entry points consumed by the flake:
#
#   loomTests — `loom gate verify` driven by `craneLib.mkCargoDerivation`
#               with LOOM_VERIFY_TIERS=check,test so the deterministic
#               tiers run inside the Nix build sandbox. The `[system]`
#               tier is excluded because its verifiers shell out to
#               `nix run` / `podman`, neither of which is available
#               under `nix flake check`.
#
#   loom-smoke — Linux-only `writeShellApplication` wrapping the
#               container smoke harness (`tests/run-tests.sh`). On
#               Darwin a stub script exits 0 with the documented
#               "container smoke not available on Darwin" message so
#               `nix run .#test` is a no-op rather than an error.
#
# Spec: specs/tests.md § Nix Integration / Cross-platform / CI integration.
{ pkgs, loomPackage, ... }:

let
  inherit (pkgs) lib;
  inherit (lib) optionalAttrs;
  isLinux = pkgs.stdenv.hostPlatform.isLinux;
  inherit (loomPackage)
    craneLib
    stagedSrc
    cargoArtifacts
    bin
    ;

  loomTests = craneLib.mkCargoDerivation {
    pname = "tests";
    version = "0.0.0";
    src = stagedSrc;
    inherit cargoArtifacts;
    doCheck = true;
    nativeBuildInputs = [
      pkgs.git
      pkgs.cargo-nextest
      bin
    ];
    buildPhaseCargoCommand = ''
      cargo --version
      cargo nextest --version
      loom --version
    '';
    preCheck = ''
      export HOME=$(mktemp -d)
    '';
    checkPhaseCargoCommand = ''
      LOOM_VERIFY_TIERS=check,test loom gate verify
    '';
  };

  smokeApp = pkgs.writeShellApplication {
    name = "test";
    runtimeInputs = [
      bin
      pkgs.podman
      pkgs.jq
      pkgs.git
    ];
    text = builtins.readFile ../run-tests.sh;
  };

  darwinStub = pkgs.writeShellApplication {
    name = "test";
    text = ''
      echo "container smoke not available on Darwin"
      exit 0
    '';
  };
in
{
  inherit loomTests;
}
// optionalAttrs isLinux { loom-smoke = smokeApp; }
// optionalAttrs (!isLinux) { loom-smoke = darwinStub; }

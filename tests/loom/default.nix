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
      pkgs.cacert
      bin
    ];
    buildPhaseCargoCommand = ''
      cargo --version
      cargo nextest --version
      loom --version
    '';
    # genai builds a reqwest TLS client eagerly; sandbox needs a CA bundle.
    preCheck = ''
      export HOME=$(mktemp -d)
      export GIT_CONFIG_GLOBAL="$PWD/tests/fixtures/git/test-gitconfig"
      export GIT_CONFIG_SYSTEM=/dev/null
      export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
    '';
    # `--tree` (every verifier, no file filter) is the explicit scope for a
    # git-less build sandbox: the source artifact has no `.git`, so a
    # `--diff`-based scope can't resolve and `loom gate` now fails loudly
    # rather than silently degrading. `--tree` matches the prior whole-tree
    # behavior exactly (an empty file filter ran every verifier).
    checkPhaseCargoCommand = ''
      LOOM_VERIFY_TIERS=check,test loom gate verify --tree
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

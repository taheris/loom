# tests/loom/default.nix
#
# Builds the two test entry points consumed by the flake:
#
#   loomTests — deterministic `loom gate` tiers driven by
#               `craneLib.mkCargoDerivation`. It runs explicit
#               `loom gate check --tree` and `loom gate test --tree`
#               commands inside the Nix build sandbox. The `[system]`
#               tier is excluded because its verifiers shell out to
#               `nix run` / `podman`, neither of which is available
#               inside the Nix build sandbox.
#
#   loom-smoke — Linux-only `writeShellApplication` wrapping the
#               container smoke harness (`tests/run-tests.sh`). On
#               Darwin a stub script exits 0 with the documented
#               "container smoke not available on Darwin" message so
#               `nix run .#smoke` is a no-op rather than an error.
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
    # `--tree` is the explicit scope for each deterministic tier in a
    # git-less build sandbox: the source artifact has no `.git`, so a
    # `--diff`-based scope can't resolve and `loom gate` fails loudly
    # rather than silently degrading. Running `check` and `test` separately
    # excludes `[system]` without an environment override.
    checkPhaseCargoCommand = ''
      loom gate check --tree
      loom gate test --tree
    '';
  };

  smokeApp = pkgs.writeShellApplication {
    name = "smoke";
    runtimeInputs = [
      bin
      pkgs.podman
      pkgs.jq
      pkgs.git
    ];
    text = builtins.readFile ../run-tests.sh;
  };

  darwinStub = pkgs.writeShellApplication {
    name = "smoke";
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

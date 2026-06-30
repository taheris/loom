# Build the loom Cargo workspace.
#
# Exposed as `legacyPackages.${system}.lib.mkLoom` so consumers (notably
# wrix dogfood + any flake that wires loom into a wrix sandbox in
# "direct" mode) can call it with their own `pkgs`/`crane`/`fenix` — for
# example with `linuxPkgs` on a Darwin host to get a Linux-built
# `loom-direct-runner` to drop into a wrix sandbox image.
{
  pkgs,
  crane,
  fenix,
  src,
  toolchain ? null,
}:

let
  inherit (pkgs) lib;
  inherit (lib)
    cleanSourceWith
    concatStringsSep
    hasInfix
    hasSuffix
    mapAttrsToList
    ;

  fenixPkgs = fenix.packages.${pkgs.stdenv.hostPlatform.system};
  resolvedToolchain =
    if toolchain != null then toolchain else fenixPkgs.combine [ fenixPkgs.stable.defaultToolchain ];
  craneLib = (crane.mkLib pkgs).overrideToolchain (_: resolvedToolchain);

  # Keep template assets, built-in skill packages, and snapshot files
  # alongside the Cargo workspace — crane's default filter would exclude them.
  srcFilter =
    path: type:
    (craneLib.filterCargoSources path type)
    || (hasInfix "/loom-templates/templates/" path)
    || (hasInfix "/loom-skills/builtin/" path)
    || (hasSuffix ".snap" path);

  cleanedSrc = cleanSourceWith {
    inherit src;
    filter = srcFilter;
  };

  commonArgs = {
    src = cleanedSrc;
    cargoLock = "${src}/Cargo.lock";
    nativeBuildInputs = [ pkgs.git ];
    meta = {
      description = "Rust workflow orchestrator";
      mainProgram = "loom";
    };
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  # Specs, mock binaries, and any other non-Cargo inputs the tests and
  # `[check]`-tier verifiers read from the workspace root.
  extraSrcs = {
    "tests/mock-pi" = "${src}/tests/mock-pi";
    "tests/mock-claude" = "${src}/tests/mock-claude";
    "tests/inbox-bridge" = "${src}/tests/inbox-bridge";
    "tests/loom" = "${src}/tests/loom";
    "tests/default.nix" = "${src}/tests/default.nix";
    "tests/run-tests.sh" = "${src}/tests/run-tests.sh";
    "tests/judges" = "${src}/tests/judges";
    "tests/fixtures" = "${src}/tests/fixtures";
    "specs" = "${src}/specs";
    "docs" = "${src}/docs";
    "nix/flake" = "${src}/nix/flake";
    "nix/workspace.nix" = "${src}/nix/workspace.nix";
    "scripts" = "${src}/scripts";
    "bin/pre-push-checks" = "${src}/bin/pre-push-checks";
    ".pre-commit-config.yaml" = "${src}/.pre-commit-config.yaml";
  };

  stagedSrc = pkgs.runCommand "loom-src-with-extras" { } (
    ''
      cp -r ${cleanedSrc} $out
      chmod -R u+w $out
    ''
    + concatStringsSep "\n" (
      mapAttrsToList (rel: abs: ''
        mkdir -p "$(dirname "$out/${rel}")"
        rm -rf "$out/${rel}"
        cp -r ${abs} "$out/${rel}"
      '') extraSrcs
    )
  );

  bin = craneLib.buildPackage (
    commonArgs
    // {
      inherit cargoArtifacts;
      doCheck = false;
    }
  );

  clippy = craneLib.cargoClippy (
    commonArgs
    // {
      src = stagedSrc;
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-targets";
    }
  );

  nextest = craneLib.cargoNextest (
    commonArgs
    // {
      src = stagedSrc;
      inherit cargoArtifacts;
      nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
        pkgs.cacert
        pkgs.flock
        pkgs.jq
        pkgs.openssh
      ];
      # genai builds a reqwest TLS client eagerly; sandbox needs a CA bundle.
      preCheck = ''
        export HOME=$(mktemp -d)
        export GIT_CONFIG_GLOBAL="$PWD/tests/fixtures/git/test-gitconfig"
        export GIT_CONFIG_SYSTEM=/dev/null
        export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
      '';
    }
  );
in
{
  inherit
    bin
    clippy
    nextest
    cargoArtifacts
    craneLib
    stagedSrc
    ;
  toolchain = resolvedToolchain;
}

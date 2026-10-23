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
    concatStringsSep
    hasInfix
    hasSuffix
    mapAttrsToList
    optionalAttrs
    optionalString
    ;

  fenixPkgs = fenix.packages.${pkgs.stdenv.hostPlatform.system};
  resolvedToolchain =
    if toolchain != null then toolchain else fenixPkgs.combine [ fenixPkgs.stable.defaultToolchain ];
  craneLib = ((crane.mkLib pkgs).overrideToolchain (_: resolvedToolchain)).overrideScope (
    import ./nextest-filtering-overlay.nix
  );

  # Keep template assets, built-in skills, embedded shell scripts, and snapshots
  # alongside the Cargo workspace — crane's default filter would exclude them.
  srcFilter =
    path: type:
    (craneLib.filterCargoSources path type)
    || (hasInfix "/loom-templates/templates/" path)
    || (hasInfix "/loom-skill/builtin/" path)
    || (hasInfix "/tests/fixtures/quint/" path)
    || (hasSuffix ".sh" path)
    || (hasSuffix ".jsonl" path)
    || (hasSuffix ".snap" path);

  checked = import ./verification.nix;
  workspaceSource = checked.resource {
    root = src;
    name = "loom-cargo-source";
    filter = srcFilter;
  };
  cleanedSrc = workspaceSource.path;
  evaluation = [
    (checked.resource {
      root = src;
      origin = "nix";
    })
  ]
  ++ lib.optional (builtins.pathExists "${toString src}/rust-toolchain.toml") (
    checked.resource {
      root = src;
      origin = "rust-toolchain.toml";
    }
  );

  manifestSource = if src._isLibCleanSourceWith or false then src.origSrc else src;
  manifestPath =
    relative:
    assert
      !(src._isLibCleanSourceWith or false)
      || src.filter "${toString manifestSource}/${relative}" "regular";
    "${toString manifestSource}/${relative}";

  commonArgs = {
    src = cleanedSrc;
    cargoLock = builtins.path {
      path = manifestPath "Cargo.lock";
      name = "Cargo.lock";
    };
    cargoToml = builtins.path {
      path = manifestPath "Cargo.toml";
      name = "Cargo.toml";
    };
    cargoVendorDir = craneLib.vendorCargoDeps {
      inherit (commonArgs) cargoLock cargoToml;
    };
    nativeBuildInputs = [ pkgs.git ];
    LOOM_NATIVE_NIX_BIN = "${pkgs.nix}/bin/nix";
    meta = {
      description = "Rust workflow orchestrator";
      mainProgram = "loom";
    };
  }
  // optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
    LOOM_NATIVE_FLOCK_BIN = "${pkgs.util-linux}/bin/flock";
    LOOM_NATIVE_SHELL_BIN = pkgs.runtimeShell;
    LOOM_NATIVE_STAT_BIN = "${pkgs.coreutils}/bin/stat";
  };

  hookRunner = import ./prek.nix { inherit pkgs; };

  lockedNextest =
    lib.findFirst (package: package.name == "nextest-filtering")
      (throw "Cargo.lock does not contain nextest-filtering")
      (builtins.fromTOML (builtins.readFile commonArgs.cargoLock)).package;
  patchedNextest = craneLib.downloadCargoPackage lockedNextest;
  cargoConfig =
    (pkgs.writeText "loom-cargo-patches.toml" ''
      paths = [ "${patchedNextest}" ]

      [env]
      LOOM_PREK_BIN = { value = "${hookRunner}/bin/prek", force = true }
      LOOM_ISSUES_BD_BIN = { value = "${pkgs.beads}/bin/bd", force = true }
      LOOM_NATIVE_NIX_BIN = { value = "${pkgs.nix}/bin/nix", force = true }
      ${optionalString pkgs.stdenv.hostPlatform.isLinux ''
        LOOM_NATIVE_FLOCK_BIN = { value = "${pkgs.util-linux}/bin/flock", force = true }
        LOOM_NATIVE_SHELL_BIN = { value = "${pkgs.runtimeShell}", force = true }
        LOOM_NATIVE_STAT_BIN = { value = "${pkgs.coreutils}/bin/stat", force = true }
      ''}
    '').overrideAttrs
      {
        passthru = {
          nextestVersion = lockedNextest.version;
          nextestChecksum = lockedNextest.checksum;
        };
      };

  # Preserve the original path for Crane's composed manifest filters. Refiltering
  # an already computed builtins.path fails during cold, read-only evaluation.
  cargoArtifacts = craneLib.buildDepsOnly (
    commonArgs
    // {
      src = lib.cleanSourceWith {
        inherit src;
        name = "loom-cargo-source";
        filter = srcFilter;
      };
    }
  );

  # Specs, mock binaries, and any other non-Cargo inputs the tests and
  # `[check]`-tier verifiers read from the workspace root.
  extraResources =
    map
      (
        origin:
        checked.resource {
          root = src;
          inherit origin;
          name = builtins.baseNameOf origin;
        }
      )
      [
        ".config/nextest.toml"
        ".cargo/config.toml"
        "rust-toolchain.toml"
        "AGENTS.md"
        "loom.toml"
        "wrix.toml"
        ".envrc"
        "flake.nix"
        "flake.lock"
        "tests"
        "crates/loom-protocol/tests/ui"
        "crates/loom-skill/tests/ui"
        "specs"
        "docs"
        "nix"
        "scripts"
        "bin/pre-push-checks"
        "bin/loom"
        ".gitignore"
        ".pre-commit-config.yaml"
      ];
  extraSrcs = builtins.listToAttrs (
    map ({ origin, path, ... }: {
      name = origin;
      value = path;
    }) extraResources
  );
  testResources = [ workspaceSource ] ++ extraResources;

  stagedSrc = pkgs.runCommand "loom-src-with-extras" { } (
    ''
      cp -r ${cleanedSrc} $out
      chmod -R u+w $out
    ''
    + concatStringsSep "\n" (
      mapAttrsToList (rel: path: ''
        mkdir -p "$(dirname "$out/${rel}")"
        rm -rf "$out/${rel}"
        cp -r ${path} "$out/${rel}"
      '') extraSrcs
    )
  );

  bin = checked.check {
    inherit evaluation;
    resources = [ workspaceSource ];
    build =
      _:
      craneLib.buildPackage (
        commonArgs
        // {
          LOOM_PREK_BIN = "${hookRunner}/bin/prek";
          inherit cargoArtifacts;
          doCheck = false;
        }
      );
  };

  verificationBuild = checked.cargo {
    inherit craneLib evaluation;
    operation = "build";
    resources = testResources;
    args = commonArgs // {
      LOOM_PREK_BIN = "${hookRunner}/bin/prek";
      src = stagedSrc;
      inherit cargoArtifacts;
    };
  };

  clippy = checked.cargo {
    inherit craneLib evaluation;
    operation = "lint";
    resources = [ workspaceSource ];
    args = commonArgs // {
      LOOM_PREK_BIN = "${hookRunner}/bin/prek";
      LOOM_ISSUES_BD_BIN = "${pkgs.beads}/bin/bd";
      inherit cargoArtifacts;
    };
  };

  clippyAllTargets = checked.cargo {
    inherit craneLib evaluation;
    operation = "lint-all-targets";
    resources = testResources;
    args = commonArgs // {
      LOOM_PREK_BIN = "${hookRunner}/bin/prek";
      LOOM_ISSUES_BD_BIN = "${pkgs.beads}/bin/bd";
      src = stagedSrc;
      inherit cargoArtifacts;
    };
  };

  nextest = checked.cargo {
    inherit craneLib evaluation;
    operation = "test";
    resources = testResources;
    args = commonArgs // {
      LOOM_PREK_BIN = "${hookRunner}/bin/prek";
      LOOM_ISSUES_BD_BIN = "${pkgs.beads}/bin/bd";
      src = stagedSrc;
      inherit cargoArtifacts;
      nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
        pkgs.cacert
        pkgs.cargo-nextest
        pkgs.dolt
        pkgs.flock
        pkgs.jq
        pkgs.openssh
        pkgs.sccache
      ];
      GIT_CONFIG_GLOBAL = "${stagedSrc}/tests/fixtures/git/test-gitconfig";
      SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    };
  };
in
{
  inherit
    bin
    verificationBuild
    clippy
    clippyAllTargets
    nextest
    cargoArtifacts
    cargoConfig
    hookRunner
    craneLib
    stagedSrc
    workspaceSource
    testResources
    ;
  cargoVendorDir = commonArgs.cargoVendorDir;
  toolchain = resolvedToolchain;
}

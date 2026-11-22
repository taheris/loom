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
  nativeTestInputs ? [ ],
  nativeTestSources ? [ ],
  cargoToml ? null,
  cargoLock ? null,
  cargoVendorDir ? null,
}:

let
  inherit (pkgs) lib;
  inherit (lib)
    concatStringsSep
    getBin
    hasInfix
    hasPrefix
    hasSuffix
    removePrefix
    mapAttrsToList
    optional
    optionalAttrs
    optionalString
    ;

  fenixPkgs = fenix.packages.${pkgs.stdenv.hostPlatform.system};
  resolvedToolchain =
    if toolchain != null then toolchain else fenixPkgs.combine [ fenixPkgs.stable.defaultToolchain ];
  craneLib = ((crane.mkLib pkgs).overrideToolchain (_: resolvedToolchain)).overrideScope (
    lib.composeExtensions (import ./nextest-filtering-overlay.nix) (
      _: previous:
      optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
        mkCargoDerivation =
          args:
          previous.mkCargoDerivation (
            args
            // {
              nativeBuildInputs = lib.unique ((args.nativeBuildInputs or [ ]) ++ darwinCompilerHooks);
            }
          );
      }
    )
  );

  # Keep template assets, built-in skills, embedded shell scripts, and snapshots
  # alongside the Cargo workspace — crane's default filter would exclude them.
  srcFilter =
    path: type:
    (craneLib.filterCargoSources path type)
    || (hasInfix "/loom-templates/templates/" path)
    || (hasInfix "/loom-skill/builtin/" path)
    || (hasInfix "/tests/fixtures/quint/" path)
    || (hasInfix "/loom-verification/tests/fixtures/" path)
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
  # The release binary embeds these external scripts, not the repository's
  # shell test campaigns. Test compilation/execution retain workspaceSource.
  binarySource = checked.resource {
    root = src;
    name = "loom-binary-source";
    filter =
      path: type:
      let
        original = if src._isLibCleanSourceWith or false then src.origSrc else src;
        relative = removePrefix "${toString original}/" path;
      in
      srcFilter path type
      && (
        type == "directory"
        || !(builtins.any (prefix: hasPrefix prefix relative) [
          "tests/"
          "scripts/"
          "nix/"
        ])
        || builtins.elem relative [
          "nix/private-fixture-tests.sh"
          "nix/retain-fixture-inputs.sh"
          "nix/test-scheduling.sh"
          "scripts/worker-native-preflight.sh"
        ]
      );
  };
  compilerSource = checked.resource {
    root = src;
    name = "loom-test-compiler-source";
    filter =
      path: type:
      let
        original = if src._isLibCleanSourceWith or false then src.origSrc else src;
        relative = removePrefix "${toString original}/" path;
      in
      type == "directory"
      || hasPrefix "crates/" relative
      || builtins.elem relative [
        "Cargo.toml"
        "Cargo.lock"
        "clippy.toml"
        "rust-toolchain.toml"
        ".cargo/config.toml"
        ".config/nextest.toml"
        ".pre-commit-config.yaml"
        "nix/verification.nix"
        "nix/report-supervisor.sh"
        "nix/private-fixture-tests.sh"
        "nix/retain-fixture-inputs.sh"
        "nix/test-scheduling.sh"
        "bin/pre-push-checks"
        "scripts/worker-native-preflight.sh"
        "tests/fixtures/planning_prompt_interview_modes.md"
        "tests/fixtures/verifier/sandbox-results.sh"
        "tests/fixtures/quint/campaign.json"
        "tests/fixtures/quint/campaign-inputs.json"
        "tests/smoke/spec.md"
        "tests/mock-pi/pi.sh"
      ];
  };
  evaluation = [
    (checked.resource {
      root = src;
      origin = "nix";
    })
  ]
  ++ optional (builtins.pathExists "${toString src}/rust-toolchain.toml") (
    checked.resource {
      root = src;
      origin = "rust-toolchain.toml";
    }
  )
  # mkDummySrc discovers independent nested workspaces as well as Cargo members.
  # Retain the real manifest/target inputs when reconstructing compiler graphs.
  ++
    optional
      (
        let
          original = if src._isLibCleanSourceWith or false then src.origSrc else src;
          manifest = "${toString original}/tests/fixtures/catalogue-reuse/Cargo.toml";
        in
        builtins.pathExists manifest
        && (!(src._isLibCleanSourceWith or false) || src.filter manifest "regular")
      )
      (
        checked.resource {
          root = src;
          origin = "tests/fixtures/catalogue-reuse";
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
    cargoLock =
      if cargoLock != null then
        cargoLock
      else
        builtins.path {
          path = manifestPath "Cargo.lock";
          name = "Cargo.lock";
        };
    cargoToml =
      if cargoToml != null then
        cargoToml
      else
        builtins.path {
          path = manifestPath "Cargo.toml";
          name = "Cargo.toml";
        };
    cargoVendorDir =
      if cargoVendorDir != null then
        cargoVendorDir
      else
        craneLib.vendorCargoDeps {
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
      LOOM_TEST_DOLT_BIN = { value = "${dolt}/bin/dolt", force = true }
      LOOM_NATIVE_SHELL_BIN = { value = "${pkgs.runtimeShell}", force = true }
      ${optionalString pkgs.stdenv.hostPlatform.isLinux ''
        LOOM_NATIVE_FLOCK_BIN = { value = "${pkgs.util-linux}/bin/flock", force = true }
        LOOM_NATIVE_STAT_BIN = { value = "${pkgs.coreutils}/bin/stat", force = true }
      ''}
    '').overrideAttrs
      {
        passthru = {
          nextestVersion = lockedNextest.version;
          nextestChecksum = lockedNextest.checksum;
        };
      };

  # Darwin dyld may resolve deduplicated compiler inodes through another store
  # alias. Fall back to the selected toolchain, without overriding Clang's LLVM.
  darwinCompilerHooks = optional pkgs.stdenv.hostPlatform.isDarwin (
    pkgs.makeSetupHook { name = "loom-test-darwin-libraries"; } (
      pkgs.writeText "loom-test-darwin-libraries.sh" ''
        export DYLD_FALLBACK_LIBRARY_PATH="${resolvedToolchain}/lib"
      ''
    )
  );

  testCompilerHooks =
    optional pkgs.stdenv.hostPlatform.isLinux (
      pkgs.makeSetupHook { name = "loom-test-debug-compression"; } (
        pkgs.writeText "loom-test-debug-compression.sh" ''
          export NIX_LDFLAGS="''${NIX_LDFLAGS:-} --compress-debug-sections=zlib"
        ''
      )
    )
    ++ darwinCompilerHooks;

  # Preserve the original path for Crane's composed manifest filters. Refiltering
  # an already computed builtins.path fails during cold, read-only evaluation.
  dependencyArtifacts =
    CARGO_PROFILE:
    craneLib.buildDepsOnly (
      commonArgs
      // optionalAttrs (CARGO_PROFILE != "release") { inherit CARGO_PROFILE; }
      # Preserve complete compiler data in Crane's supported archive format;
      # installing another unpacked target exhausts bounded native stores.
      // optionalAttrs (CARGO_PROFILE != "release") { installCargoArtifactsMode = "use-zstd"; }
      // {
        src = lib.cleanSourceWith {
          inherit src;
          name = "loom-cargo-source";
          filter = srcFilter;
        };
      }
    );
  cargoArtifacts = dependencyArtifacts "release";
  testCargoArtifacts = dependencyArtifacts "dev";
  workspaceLibraries = map (
    member:
    let
      manifest = builtins.fromTOML (builtins.readFile (manifestSource + "/${member}/Cargo.toml"));
    in
    manifest.lib.name or (builtins.replaceStrings [ "-" ] [ "_" ] manifest.package.name)
  ) (builtins.fromTOML (builtins.readFile commonArgs.cargoToml)).workspace.members;
  writableWorkspaceArtifacts = pkgs.makeSetupHook { name = "loom-writable-workspace-artifacts"; } (
    pkgs.writeText "loom-writable-workspace-artifacts.sh" ''
      makeWorkspaceArtifactsWritable() {
        bash ${./writable-cargo-artifacts.sh} "''${CARGO_TARGET_DIR:-target}" ${lib.escapeShellArgs workspaceLibraries}
      }
      preBuildHooks+=(makeWorkspaceArtifactsWritable)
    ''
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
        "nix"
        "scripts"
        "bin/pre-push-checks"
        "bin/loom"
        ".gitignore"
        ".pre-commit-config.yaml"
      ]
    ++ [
      # The staged project needs its spec index and workflow guidance, not
      # engineering prose. Declared resources also drive affected selection.
      (checked.resource {
        root = src;
        origin = "docs";
        name = "docs";
        filter =
          let
            original = if src._isLibCleanSourceWith or false then src.origSrc else src;
          in
          path: type:
          type == "directory"
          || builtins.elem (removePrefix "${toString original}/" path) [
            "docs/README.md"
            "docs/spec-conventions.md"
            "docs/style-rules.md"
          ];
      })
    ];
  extraSrcs = builtins.listToAttrs (
    map ({ origin, path, ... }: {
      name = origin;
      value = path;
    }) extraResources
  );
  testResources = [ workspaceSource ] ++ extraResources;
  compilerResources = [ compilerSource ] ++ extraResources;

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

  bin = checked.artifact {
    runtime = "native-process";
    inherit evaluation;
    resources = [ binarySource ];
    build =
      _:
      craneLib.buildPackage (
        commonArgs
        // {
          src = binarySource.path;
          LOOM_PREK_BIN = "${hookRunner}/bin/prek";
          LOOM_BUILTIN_SOURCE_ROOT = "${binarySource.path}/crates/loom-skill";
          inherit cargoArtifacts;
          doCheck = false;
        }
        // optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          # LLVM's private ThinLTO symbol suffixes can vary between identical
          # builds. Remove only these compiler-internal symbol records, keeping
          # loaded sections, dynamic exports and ordinary Rust symbols intact.
          # Gate still rebuilds and compares the complete installed output.
          postFixup = ''
            for program in "$out"/bin/*; do
              ${pkgs.binutils-unwrapped}/bin/strip --wildcard '--strip-symbol=*.llvm.*' "$program"
            done
          '';
        }
      );
  };

  verificationBuild = checked.cargo {
    inherit craneLib evaluation;
    operation = "build";
    runtime = "native-process";
    resources = [ compilerSource ];
    args = commonArgs // {
      LOOM_PREK_BIN = "${hookRunner}/bin/prek";
      src = compilerSource.path;
      LOOM_ISSUES_BD_BIN = "${pkgs.beads}/bin/bd";
      LOOM_TEST_DOLT_BIN = "${dolt}/bin/dolt";
      LOOM_NATIVE_SHELL_BIN = pkgs.runtimeShell;
      CARGO_PROFILE = "dev";
      cargoArtifacts = testCompilation;
      nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ writableWorkspaceArtifacts ];
    };
  };

  clippy = checked.cargo {
    inherit craneLib evaluation;
    operation = "lint";
    runtime = "native-process";
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
    runtime = "native-process";
    resources = testResources;
    args = commonArgs // {
      LOOM_PREK_BIN = "${hookRunner}/bin/prek";
      LOOM_ISSUES_BD_BIN = "${pkgs.beads}/bin/bd";
      LOOM_TEST_DOLT_BIN = "${dolt}/bin/dolt";
      src = stagedSrc;
      inherit cargoArtifacts;
    };
  };

  dolt = import ./dolt.nix { inherit pkgs; };
  nativeSmokeFixture = import ./tests/native-smoke-fixture.nix { inherit pkgs; };
  # Metadata-only query; the supplied compiler closure is rooted below.
  queryCompiler = builtins.unsafeDiscardStringContext (toString resolvedToolchain);
  queryPackage = (craneLib.overrideToolchain queryCompiler).buildPackage {
    pname = "native-probe";
    version = "0.1.0";
    src = cleanedSrc;
    cargoArtifacts = null;
    cargoVendorDir = commonArgs.cargoVendorDir;
  };
  testInputs = map getBin (
    [
      pkgs.cacert
      pkgs.cargo-nextest
      pkgs.findutils
      dolt
      pkgs.beads
      pkgs.flock
      pkgs.jq
      pkgs.openssh
      pkgs.sccache
      (getBin pkgs.nix)
      (import ./verification-tool.nix { inherit pkgs; })
      (getBin pkgs.bash)
      hookRunner
      # Integration tests consume the catalogue's actual Cargo-built CLI, not
      # another release-mode workspace build retained as a runtime dependency.
    ]
    ++ nativeTestInputs
  );
  # Preparation carries executable/vendor inputs, not the staged test source
  # graph. Fixtures are cheap offline recipes over these retained tools.
  preparationInputs = pkgs.closureInfo {
    rootPaths =
      testInputs
      ++ testCompilerHooks
      ++ queryPackage.nativeBuildInputs
      ++ queryPackage.buildInputs
      ++ map lib.getDev (queryPackage.nativeBuildInputs ++ queryPackage.buildInputs)
      ++ nativeTestSources
      ++ [
        queryPackage.stdenv
        resolvedToolchain
        pkgs.stdenvNoCC
        cargoConfig
        commonArgs.cargoVendorDir
        cargoArtifacts
        testCargoArtifacts
      ];
  };
  testRegistration = pkgs.closureInfo {
    rootPaths =
      testInputs
      ++ testCompilerHooks
      ++ queryPackage.nativeBuildInputs
      ++ queryPackage.buildInputs
      ++ [ queryPackage.stdenv ]
      ++ nativeTestSources
      ++ map (resource: resource.path) extraResources
      ++
        map
          (
            name:
            builtins.path {
              path = pkgs.path + "/pkgs/stdenv/generic/${name}";
              inherit name;
            }
          )
          [
            "builder.sh"
            "default-builder.sh"
            "setup.sh"
            "source-stdenv.sh"
          ]
      ++ [
        resolvedToolchain
        pkgs.stdenv
        pkgs.stdenvNoCC
        stagedSrc
        cleanedSrc
        # Re-importing an unregistered source can replace a live test cwd.
        compilerSource.path
        testCompilation
        commonArgs.cargoLock
        commonArgs.cargoToml
        cargoConfig
        commonArgs.cargoVendorDir
        cargoArtifacts
        testCargoArtifacts
        nativeRustFixture
        nativeSmokeFixture
        ./report-supervisor.sh
        ./private-fixture-tests.sh
        ./retain-fixture-inputs.sh
        ./test-scheduling.sh
      ];
  };

  checkArgs = commonArgs // {
    LOOM_NATIVE_SHELL_BIN = pkgs.runtimeShell;
    LOOM_PREK_BIN = "${hookRunner}/bin/prek";
    LOOM_ISSUES_BD_BIN = "${pkgs.beads}/bin/bd";
    LOOM_TEST_DOLT_BIN = "${dolt}/bin/dolt";
    src = stagedSrc;
    inherit cargoArtifacts;
    nativeBuildInputs = map getBin (
      commonArgs.nativeBuildInputs
      ++ [
        pkgs.cacert
        pkgs.cargo-nextest
        dolt
        pkgs.flock
        pkgs.jq
        pkgs.openssh
        pkgs.sccache
      ]
    );
    GIT_CONFIG_GLOBAL = "${stagedSrc}/tests/fixtures/git/test-gitconfig";
    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  };

  # Carry explicit metadata/vendor inputs so Crane never reads unrealized stagedSrc.
  mkCheck =
    { ... }@overrides:
    assert !(overrides ? src);
    checked.check {
      inherit evaluation;
      resources = testResources;
      build = _: craneLib.cargoNextest (checkArgs // overrides);
    };

  nextest = checked.cargo {
    inherit craneLib evaluation;
    operation = "test";
    runtime = if pkgs.stdenv.hostPlatform.isDarwin then "native-store" else "native-process";
    resources = compilerResources;
    compilationInputs =
      commonArgs.nativeBuildInputs
      ++ [
        pkgs.cargo-nextest
        pkgs.jq
      ]
      ++ testCompilerHooks;
    args = checkArgs // {
      src = compilerSource.path;
      LOOM_TEST_SOURCE_ROOT = stagedSrc;
      CARGO_PROFILE = "dev";
      cargoArtifacts = testCargoArtifacts;
      nativeBuildInputs =
        commonArgs.nativeBuildInputs
        ++ [
          pkgs.cargo-nextest
          pkgs.jq
        ]
        ++ testCompilerHooks;
      LOOM_TEST_RUNTIME_PATH = lib.makeBinPath testInputs;
      LOOM_TEST_NIX_REGISTRATION =
        if pkgs.stdenv.hostPlatform.isDarwin then "" else "${testRegistration}/registration";
      LOOM_TEST_NIX_INPUTS = if pkgs.stdenv.hostPlatform.isDarwin then [ ] else nativeTestSources;
      LOOM_NATIVE_RUST_FIXTURE = nativeRustFixture;
      LOOM_NATIVE_SMOKE_FIXTURE = nativeSmokeFixture;
      GIT_CONFIG_GLOBAL = "${stagedSrc}/tests/fixtures/git/test-gitconfig";
      SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    };
  };
  # The checked walk integration binary constructs canonical Nix graphs; the
  # complete native-store producer owns it, not this confined companion.
  confinedTests = checked.check {
    inherit evaluation;
    runtime = "native-process";
    resources = testResources;
    build =
      _:
      craneLib.mkCargoDerivation (
        checkArgs
        // {
          pname = "loom-confined-runtime";
          cargoArtifacts = null;
          nativeBuildInputs = checkArgs.nativeBuildInputs ++ testCompilerHooks;
          LOOM_TEST_RUNTIME_PATH = lib.makeBinPath testInputs;
          LOOM_TEST_SCHEDULING = ./test-scheduling.sh;
          NEXTEST_EXPERIMENTAL_LIBTEST_JSON = "1";
          doCheck = false;
          doInstallCargoArtifacts = false;
          installPhaseCommand = ''mkdir -p "$out"'';
          buildPhaseCargoCommand = ''
            export HOME=$(mktemp -d)
            export LOOM_TEST_SOURCE_ROOT="$PWD"
            export GIT_CONFIG_SYSTEM=/dev/null
            git init -q
            git add --all
            git -c user.name='Loom native suite' -c user.email='native@example.invalid' -c commit.gpgsign=false commit -q -m 'Capture native suite source'
            bash ${./private-fixture-tests.sh} --build-campaign cargo-nextest nextest run \
              --cargo-metadata ${testCompilation}/cargo.json \
              --binaries-metadata ${testCompilation}/binaries.json \
              --workspace-remap "$PWD" --target-dir-remap ${testCompilation}/target \
              --ignore-default-filter --no-fail-fast --no-tests=fail --message-format libtest-json-plus \
              -E 'package(=loom-agent) | package(=loom-events) | package(=loom-issues) | package(=loom-protocol) | package(=loom-render) | package(=loom-shared) | package(=loom-skill) | package(=loom-templates) | package(=loom-tune) | (package(=loom-walk) & !binary(=checked)) | test(test_support::native::) | test(verifier_deadline_terminates_and_reaps_the_running_process)'
          '';
        }
      );
  };
  testCompilation = nextest.compilation;
  nativeRustFixture = import ./tests/native-rust-fixture.nix {
    inherit pkgs crane;
    toolchain = resolvedToolchain;
    compilerHooks = testCompilerHooks;
    nextest = pkgs.cargo-nextest;
  };
in
{
  inherit
    bin
    verificationBuild
    clippy
    clippyAllTargets
    confinedTests
    nextest
    cargoArtifacts
    testCargoArtifacts
    testCompilerHooks
    writableWorkspaceArtifacts
    testCompilation
    cargoConfig
    dolt
    hookRunner
    craneLib
    stagedSrc
    workspaceSource
    compilerSource
    compilerResources
    testResources
    testRegistration
    preparationInputs
    checkArgs
    mkCheck
    ;
  inherit (commonArgs) cargoToml cargoLock cargoVendorDir;
  toolchain = resolvedToolchain;
}

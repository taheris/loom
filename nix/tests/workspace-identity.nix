{
  pkgs,
  crane,
  fenix,
  loom,
  src,
}:
let
  inherit (pkgs.lib) cleanSourceWith optionals optionalString;
  construct =
    overrides:
    import ../workspace.nix (
      {
        inherit
          pkgs
          crane
          fenix
          src
          ;
        inherit (loom) toolchain;
      }
      // overrides
    );
  without =
    relative:
    cleanSourceWith {
      inherit src;
      filter = path: _: path != "${toString src}/${relative}";
    };
  baseline = construct { };
  runtimeTool = pkgs.writeShellScriptBin "runtime-only-control" "exit 1";
  runtime = construct { nativeTestInputs = [ runtimeTool ]; };
  readme = construct { src = without "README.md"; };
  docs = construct { src = without "docs/README.md"; };
  shellTest = construct { src = without "tests/verify/agent-landing.sh"; };
  nixTemplate = construct { src = without "nix/workspace.nix"; };
  embeddedScript = construct { src = without "scripts/worker-native-preflight.sh"; };
  fixtureRunner = construct { src = without "nix/private-fixture-tests.sh"; };
  rust = construct { src = without "crates/loom-gate/src/provider/tracked.rs"; };
  manifest = construct { src = without "crates/loom-events/Cargo.toml"; };
  toolchain = construct {
    toolchain = loom.toolchain.overrideAttrs (old: {
      name = "${old.name}-identity-control";
    });
  };
  patch = construct {
    crane = crane // {
      mkLib =
        packages:
        (crane.mkLib packages).overrideScope (
          _: previous: {
            downloadCargoPackage =
              args:
              (previous.downloadCargoPackage args).overrideAttrs (_: {
                LOOM_PATCH_IDENTITY_CONTROL = "changed";
              });
          }
        );
    };
  };
  identity =
    workspace:
    map (value: value.drvPath) [
      workspace.cargoArtifacts
      workspace.bin
      workspace.stagedSrc
      workspace.clippy
      workspace.nextest
    ];
in
assert baseline.bin.drvPath == loom.bin.drvPath;
assert identity baseline == identity readme;
assert baseline.cargoArtifacts.drvPath == rust.cargoArtifacts.drvPath;
assert baseline.bin.drvPath != rust.bin.drvPath;
assert baseline.stagedSrc.drvPath != rust.stagedSrc.drvPath;
assert baseline.cargoArtifacts.drvPath == docs.cargoArtifacts.drvPath;
assert baseline.bin.drvPath == docs.bin.drvPath;
assert baseline.stagedSrc.drvPath != docs.stagedSrc.drvPath;
assert baseline.bin.drvPath == shellTest.bin.drvPath;
assert baseline.nextest.drvPath != shellTest.nextest.drvPath;
assert baseline.bin.drvPath != embeddedScript.bin.drvPath;
assert baseline.bin.drvPath != fixtureRunner.bin.drvPath;
assert baseline.testCompilation.drvPath == docs.testCompilation.drvPath;
assert baseline.testCompilation.drvPath == nixTemplate.testCompilation.drvPath;
assert baseline.nextest.drvPath != nixTemplate.nextest.drvPath;
assert baseline.testCompilation.drvPath == runtime.testCompilation.drvPath;
assert baseline.testCargoArtifacts.installCargoArtifactsMode == "use-zstd";
assert baseline.testCompilation.prePatch == ''export CARGO_TARGET_DIR="$out/target"'';
assert !baseline.testCompilation.doInstallCargoArtifacts;
assert
  baseline.nextest.loom.reporter.prePatch
  == ''postPatchHooks=("''${postPatchHooks[@]/inheritCargoArtifacts/}")'';
assert baseline.testCompilation.dontFixup;
assert baseline.testCompilation.NIX_NO_SELF_RPATH == 1;
assert baseline.nextest.drvPath != runtime.nextest.drvPath;
assert builtins.elem "${runtimeTool}/bin" (
  pkgs.lib.splitString ":" runtime.nextest.LOOM_TEST_RUNTIME_PATH
);
assert
  map toString runtime.nextest.nativeBuildInputs
  == map toString runtime.testCompilation.nativeBuildInputs;
assert
  !(builtins.elem (toString runtimeTool) (map toString runtime.testCompilation.nativeBuildInputs));
assert baseline.nextest.drvPath != docs.nextest.drvPath;
assert baseline.testCompilation.drvPath != rust.testCompilation.drvPath;
assert baseline.testCompilation.drvPath != toolchain.testCompilation.drvPath;
assert baseline.testCompilation.drvPath != patch.testCompilation.drvPath;
assert baseline.cargoArtifacts.drvPath != manifest.cargoArtifacts.drvPath;
assert baseline.cargoArtifacts.drvPath != toolchain.cargoArtifacts.drvPath;
assert baseline.cargoArtifacts.drvPath != patch.cargoArtifacts.drvPath;
assert baseline.cargoConfig != patch.cargoConfig;
pkgs.runCommand "workspace-build-identity"
  {
    nativeBuildInputs = optionals pkgs.stdenv.hostPlatform.isDarwin (
      baseline.testCompilerHooks ++ [ pkgs.stdenv.cc ]
    );
  }
  ''
    cmp ${baseline.compilerSource.path}/clippy.toml ${src + "/clippy.toml"}
    cmp ${baseline.cargoArtifacts.cargoLock} ${src + "/Cargo.lock"}
    ${optionalString pkgs.stdenv.hostPlatform.isDarwin ''
      test "''${DYLD_FALLBACK_LIBRARY_PATH:?compiler runtime hook was not applied}" = "${baseline.toolchain}/lib"
      ${baseline.toolchain}/bin/rustc --version
      ${baseline.toolchain}/bin/rustdoc --version
      ${pkgs.stdenv.cc}/bin/cc --version
    ''}
    touch "$out"
  ''

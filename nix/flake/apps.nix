# Loom-owned test entrypoints. Wrix runtime/backend conformance belongs upstream.
{ inputs, ... }:
{
  perSystem =
    {
      pkgs,
      loom,
      config,
      system,
      smokeProfileManifest,
      smokeSandbox,
      smokeServiceImage,
      wrixLib,
      wrixLauncher,
      ...
    }:
    let
      inherit (pkgs.lib) optionalString;
      checked = import ../verification.nix;
      nativePackage = import ../native-package.nix;
      source =
        origin:
        checked.resource {
          root = ../..;
          inherit origin;
        };
      evaluation = import ../native-context.nix;
      verificationTool = import ../verification-tool.nix {
        inherit pkgs;
        buildTestBinaries = false;
      };
      testsDeriv = import ../../tests/default.nix {
        inherit
          pkgs
          smokeProfileManifest
          smokeSandbox
          smokeServiceImage
          wrixLauncher
          ;
        loomPackage = loom;
        smokePrekHooks = wrixLib.prekHooks;
        smokeBeads = import ../native-smoke-beads.nix {
          inherit pkgs;
          upstream = import (inputs.wrix + "/lib/beads/package.nix") { inherit pkgs; };
        };
        smokeNix = import ../native-smoke-nix.nix { inherit pkgs; };
      };
      smokeRuntime = testsDeriv.loom-smoke;
      smokeScript = source "scripts/run-smoke.sh";
      smokePreflight = checked.check {
        runtime = "native-process";
        resources = [ smokeScript ];
        inherit evaluation;
        build =
          paths:
          pkgs.writeShellApplication {
            name = "smoke";
            runtimeInputs = [ pkgs.nix ];
            text = ''
              exec ${pkgs.bash}/bin/bash ${builtins.head paths} /dev/fuse /dev/net/tun .#legacyPackages.${system}.kernel-smoke-runtime "$@"
            '';
          };
      };
      kernelSmokeApp = if pkgs.stdenv.hostPlatform.isLinux then smokePreflight else smokeRuntime;
      testCompilerInputs = [
        pkgs.stdenv.cc
        pkgs.sccache
      ];
      testRuntimeInputs = [
        pkgs.coreutils
        pkgs.findutils
        pkgs.cargo-nextest
        loom.dolt
        pkgs.git
        pkgs.flock
        pkgs.nix
        verificationTool
        pkgs.gnugrep
        pkgs.gnused
        pkgs.jq
        pkgs.beads
        pkgs.openssh
        loom.hookRunner
        # Cargo builds the CLI used by integration tests. A packaged release CLI
        # here would add a second workspace build before the suite even starts.
        loom.toolchain
      ];
      mkTestApp =
        {
          name,
          text,
          runtimeInputs ? testRuntimeInputs,
          evaluation ? [ ],
          resources ? [ ],
          compilation ? null,
        }:
        checked.check {
          runtime = "native-process";
          resources =
            resources
            ++ pkgs.lib.optionals (builtins.elem verificationTool runtimeInputs) verificationTool.loom.sources;
          evaluation = (import ../native-context.nix) ++ evaluation;
          build =
            _:
            pkgs.writeShellApplication {
              inherit name;
              extraShellCheckFlags = [ "--external-sources" ];
              text =
                pkgs.lib.concatMapStringsSep "\n" (
                  hook: "source ${hook}/nix-support/setup-hook"
                ) loom.testCompilerHooks
                + "\n"
                + builtins.readFile ../test-cargo-context.sh
                + "\n"
                + text;
              runtimeInputs = [ pkgs.bash ] ++ runtimeInputs ++ testCompilerInputs;
              runtimeEnv = {
                LOOM_TEST_COMPILATION = if compilation == null then "" else compilation;
                LOOM_TEST_TOOLCHAIN = loom.toolchain;
                LOOM_TEST_SCHEDULING = ../test-scheduling.sh;
                # Bind the mutable Cargo run to its declared source data without
                # compiling a second, release-mode CLI merely to retain inputs.
                LOOM_TEST_DECLARED_SOURCES_FILE = pkgs.writeText "loom-test-sources.json" (
                  builtins.toJSON (map (resource: { inherit (resource) origin path; }) resources)
                );
                LOOM_CARGO_PATCHES = loom.cargoConfig;
                LOOM_PREK_BIN = "${loom.hookRunner}/bin/prek";
                LOOM_ISSUES_BD_BIN = "${pkgs.beads}/bin/bd";
                LOOM_TEST_DOLT_BIN = "${loom.dolt}/bin/dolt";
                LOOM_NATIVE_NIX_BIN = "${pkgs.nix}/bin/nix";
                LOOM_NATIVE_RUST_FIXTURE = nativeRustFixture;
                LOOM_NATIVE_SMOKE_FIXTURE = nativeSmokeFixture;
              };
            };
        };
      fullScript = source "scripts/full-test.sh";
      requiredScript = source "scripts/required-test.sh";
      testAppConfig = {
        name = "test";
        text = builtins.readFile ../../scripts/full-test.sh;
        resources = loom.testResources;
        evaluation = [ fullScript ];
      };
      requiredTestAppConfig = {
        name = "test-required";
        compilation = loom.testCompilation;
        text = builtins.readFile ../../scripts/required-test.sh;
        resources = loom.testResources;
        evaluation = [ requiredScript ];
      };
      # Share compilation, not smoke verdicts, with the complete native suite.
      smokeApp = mkTestApp {
        name = "smoke";
        compilation = loom.testCompilation;
        resources = loom.compilerResources;
        text = ''
          export LOOM_TEST_SOURCE_ROOT="$PWD"
          export GIT_CONFIG_GLOBAL="$PWD/tests/fixtures/git/test-gitconfig"
          export GIT_CONFIG_SYSTEM=/dev/null
          unset WRIX_SIGNING_KEY
          cargo-nextest nextest run \
            --cargo-metadata "$LOOM_TEST_COMPILATION/cargo.json" \
            --binaries-metadata "$LOOM_TEST_COMPILATION/binaries.json" \
            --workspace-remap "$PWD" --target-dir-remap "$LOOM_TEST_COMPILATION/target" \
            --ignore-default-filter --no-fail-fast --no-tests=fail \
            -E 'package(=loom) & binary(=spawn_dispatch) & (test(=loom_loop_bead_writes_per_bead_jsonl_log) | test(=completed_bead_with_renamed_builtin_source_reaches_publication))'
        '';
      };
      testApp = mkTestApp testAppConfig;
      requiredTestApp = mkTestApp requiredTestAppConfig;
      decisionAdmissionRun = pkgs.writeShellScript "decision-admission-cases" ''
        set -euo pipefail
        mkdir "$TMPDIR/decision-archive"
        exec cargo-nextest nextest run \
          --archive-file ${config.packages.cargo-retention-archive}/archive.tar.zst \
          --extract-to "$TMPDIR/decision-archive" --workspace-remap "$PWD" \
          --no-fail-fast --ignore-default-filter \
          -E 'package(=loom-workflow) & (test(decision_wait) | test(native_controller_counts_one_noop_source_and_five_distinct_decisions) | test(integrity_extension_requires_explicit_reusable_source_readmission))'
      '';
      decisionAdmissionApp = mkTestApp {
        name = "test-decision-admission";
        runtimeInputs = testRuntimeInputs ++ [ pkgs.beads ];
        text = ''
          exec bash ${../private-fixture-tests.sh} --test-process ${decisionAdmissionRun}
        '';
      };
      nativeUnitDriver = loom.craneLib.mkCargoDerivation {
        pname = "loom-native-unit-driver";
        version = "0.1.0";
        doInstallCargoArtifacts = false;
        LOOM_PREK_BIN = "${loom.hookRunner}/bin/prek";
        # These examples compile production targets, not the CLI integration
        # binary. The app still stages its complete runtime source below.
        src = loom.compilerSourceWithoutCli.path;
        inherit (loom) cargoArtifacts cargoVendorDir;
        nativeBuildInputs = [ pkgs.git ];
        preBuild = "export HOME=$(mktemp -d)";
        buildPhaseCargoCommand = ''
          cargo build --release -p loom-gate --example native-isolation --example native-rust --example native-reported
        '';
        installPhaseCommand = ''
          install -Dm755 target/release/examples/native-isolation "$out/bin/native-isolation"
          install -Dm755 target/release/examples/native-rust "$out/bin/native-rust"
          install -Dm755 target/release/examples/native-reported "$out/bin/native-reported"
        '';
        doCheck = false;
      };
      nativeRustFixture = import ../tests/native-rust-fixture.nix {
        inherit pkgs;
        inherit (inputs) crane;
        inherit (loom) toolchain;
        compilerHooks = loom.testCompilerHooks;
        nextest = pkgs.cargo-nextest;
      };
      nativeSmokeFixture = import ../tests/native-smoke-fixture.nix { inherit pkgs; };
      nativeUnitsApp = mkTestApp {
        name = "test-native-units";
        resources = [ loom.compilerSourceWithoutCli ];
        runtimeInputs = [
          pkgs.coreutils
          pkgs.git
          pkgs.nix
          pkgs.bash
          verificationTool
          loom.toolchain
        ]
        ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
          pkgs.systemd
          pkgs.util-linux
          pkgs.iproute2
        ];
        text = ''
          [[ $# == 0 ]] || exit 64
          fixture=$(mktemp -d)
          trap 'status=$?; if [[ $status == 0 ]]; then rm -rf "$fixture"; else printf "native verification fixture retained: %s\n" "$fixture" >&2; fi' EXIT
          cp -R ${loom.stagedSrc}/. "$fixture"
          chmod -R u+w "$fixture"
          export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
          git -C "$fixture" init -q
          git -C "$fixture" add --all
          loom-verification native-resources "$fixture"
          export XDG_STATE_HOME="$fixture/state"
          ${optionalString pkgs.stdenv.hostPlatform.isLinux ''
            cargo test -p loom-gate --features native-container-tests --lib -- --test-threads=1
          ''}
          if ${nativeUnitDriver}/bin/native-isolation > "$fixture/native-observed.jsonl"; then
            cat "$fixture/native-observed.jsonl"
            ${nativeUnitDriver}/bin/native-reported ${loom.bin}/bin/loom
            ${nativeUnitDriver}/bin/native-rust ${nativeRustFixture} ${loom.bin}/bin/loom
          else
            status=$?
            cat "$fixture/native-observed.jsonl" >&2
            exit "$status"
          fi
        '';
      };
      fixtureNix = pkgs.writeShellScriptBin "nix" ''
        case "$*" in
          "fmt -- --fail-on-change" | "build" | "flake check --no-warn-dirty") exit 0 ;;
          "build .#native-rust-fixture --offline --no-substitute --no-link --print-out-paths")
            printf '%s\n' ${pkgs.lib.escapeShellArg (toString nativeRustFixture)} ;;
          "build .#native-smoke-fixture --offline --no-substitute --no-link --print-out-paths")
            printf '%s\n' ${pkgs.lib.escapeShellArg (toString nativeSmokeFixture)} ;;
          "flake archive "* | "flake metadata "*)
            if [[ "''${LOOM_FIXTURE_INVALID_METADATA:-0}" == 1 ]]; then
              printf '{}\n'
            else
              printf '{"path":"%s","url":"path:%s"}\n' "$PWD" "$PWD"
            fi
            ;;
          "config show --json "*) printf '%s\n' '{"system":{"value":"${pkgs.stdenv.hostPlatform.system}"}}' ;;
          "eval --json "*)
            [[ "$*" == *'collect "packages" ++ collect "checks"'* ]] || exit 64
            printf '[]\n'
            ;;
          *) exit 64 ;;
        esac
      '';
      fixtureRuntimeInputs = [
        pkgs.coreutils
        verificationTool
        pkgs.cargo-nextest
        pkgs.git
        fixtureNix
        loom.toolchain
      ];
      # These toolchain fixtures supply their own disposable Cargo projects;
      # their unrelated Nix stages deliberately expose no project definitions.
      fixtureTestApp = mkTestApp (
        testAppConfig
        // {
          runtimeInputs = fixtureRuntimeInputs;
          resources = [ ];
        }
      );
      fixtureRequiredTestApp = mkTestApp (
        requiredTestAppConfig
        // {
          runtimeInputs = fixtureRuntimeInputs;
          resources = [ ];
          compilation = null;
        }
      );
      bootstrapWrix = wrixLib.rustPackage.package;
      bootstrapHooks = pkgs.runCommand "bootstrap-prek-hooks" { nativeBuildInputs = [ pkgs.bash ]; } ''
        cp -R ${wrixLib.prekHooks} "$out"
        chmod -R u+w "$out"
        patchShebangs "$out"
      '';
      profileProbe = checked.check {
        inherit evaluation;
        resources = [ loom.compilerSourceWithoutCli ];
        build =
          _:
          loom.craneLib.buildPackage (
            loom.checkArgs
            // {
              pname = "loom-profile-catalogue-probe";
              src = loom.compilerSourceWithoutCli.path;
              GIT_CONFIG_GLOBAL = "/dev/null";
              cargoExtraArgs = "--package loom-driver --example profile-catalogue-probe";
              doCheck = false;
              installPhaseCommand = ''
                mapfile -t candidates < <(find target -type f -name profile-catalogue-probe)
                [[ "''${#candidates[@]}" == 1 ]]
                mkdir -p "$out/bin"
                install -m755 "''${candidates[0]}" "$out/bin/profile-catalogue-probe"
              '';
            }
          );
      };
      bootstrapScript = source "tests/sandbox/profile-bootstrap.sh";
      profileBootstrapApp = checked.check {
        runtime = "native-process";
        resources = [ bootstrapScript ] ++ profileProbe.loom.sources;
        inherit evaluation;
        build =
          paths:
          pkgs.writeShellApplication {
            name = "test-profile-bootstrap";
            runtimeInputs = [
              pkgs.git
              pkgs.nix
              pkgs.jq
              pkgs.openssh
              pkgs.coreutils
              pkgs.beads
              pkgs.gnugrep
              pkgs.diffutils
              pkgs.openssl
              bootstrapWrix
              wrixLauncher
            ];
            runtimeEnv.WRIX_PREK_HOOKS = bootstrapHooks;
            runtimeEnv.WRIX_PREK_RUNNER = "${wrixLauncher}/bin/wrix-prek";
            runtimeEnv.LOOM_WRIX_BIN = "${bootstrapWrix}/bin/wrix";
            text = ''
              [[ $# == 0 ]] || exit 64
              exec ${pkgs.bash}/bin/bash ${builtins.head paths} \
                ${loom.bin}/bin/loom ${wrixLauncher}/bin/wrix \
                ${pkgs.bash} ${pkgs.coreutils} ${pkgs.stdenv.hostPlatform.system} \
                ${profileProbe}/bin/profile-catalogue-probe
            '';
          };
      };
      fuzzApp = nativePackage (
        pkgs.writeShellApplication {
          name = "fuzz-loom";
          runtimeInputs = [
            pkgs.cargo-fuzz
            loom.toolchain
          ];
          text = ''
            if [[ $# == 0 ]]; then
              echo 'usage: nix run .#fuzz-loom -- <fuzz-target> [cargo-fuzz args...]' >&2
              exit 64
            fi
            exec cargo fuzz "$@"
          '';
        }
      );
      app =
        package: name: description:
        (import ../native-app.nix) pkgs {
          inherit package name;
          meta.description = description;
        };
    in
    {
      _module.args.profileBootstrapRegistration = pkgs.closureInfo {
        # Only the inputs to the fixture's runtime-created Nix recipes need
        # private database entries. The producer already retains its executables.
        rootPaths = [
          pkgs.bash
          pkgs.coreutils
        ];
      };
      legacyPackages.kernel-smoke-runtime = nativePackage smokeRuntime;
      packages = {
        native-rust-fixture = nativePackage nativeRustFixture;
        native-smoke-fixture = nativePackage nativeSmokeFixture;
      };
      checks.test-app-hook-runner = checked.check {
        runtime = "native-process";
        resources =
          map source [
            "tests/agent/test-app-hook-runner.sh"
            "nix/private-fixture-tests.sh"
          ]
          ++ verificationTool.loom.sources;
        evaluation =
          evaluation
          ++ [
            fullScript
            requiredScript
          ]
          ++ map source [
            "tests/smoke/result-check.sh"
            "tests/smoke/spec.md"
          ];
        build =
          paths:
          pkgs.runCommand "test-app-hook-runner"
            {
              nativeBuildInputs = [
                pkgs.git
                pkgs.coreutils
              ];
            }
            ''
              set -euo pipefail
              bash ${builtins.elemAt paths 1} --test-process bash ${builtins.elemAt paths 0} \
                ${fixtureRequiredTestApp}/bin/test-required ${fixtureTestApp}/bin/test \
                ${loom.hookRunner}/bin/prek ${pkgs.beads}/bin/bd ${verificationTool}/bin/loom-verification ${pkgs.sccache}/bin/sccache ${pkgs.nix}/bin/nix ${loom.dolt}/bin/dolt
              touch "$out"
            '';
      };
      apps = {
        test-native-units =
          app nativeUnitsApp "test-native-units"
            "Verify real resource partitions and cold/warm native unit isolation.";
        test = app testApp "test" "Run Loom's standalone native test suite.";
        test-required = app requiredTestApp "test-required" "Run Loom's native tests after lint hooks.";
        test-decision-admission =
          app decisionAdmissionApp "test-decision-admission"
            "Verify Loom decision admission.";
        test-profile-bootstrap =
          app profileBootstrapApp "test-profile-bootstrap"
            "Verify authenticated profile-factory provisioning.";
        smoke = app smokeApp "smoke" "Verify Loom launch and completion seams.";
        smoke-container = app kernelSmokeApp "smoke" "Run optional container substrate diagnostics.";
        fuzz-loom = app fuzzApp "fuzz-loom" "Run an on-demand fuzz target.";
      };
    };
}

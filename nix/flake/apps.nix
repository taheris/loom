# Loom-owned test entrypoints. Wrix runtime/backend conformance belongs upstream.
{ inputs, ... }:
{
  perSystem =
    {
      pkgs,
      loom,
      smokeProfileManifest,
      smokeSandbox,
      smokeServiceImage,
      wrixLib,
      wrixLauncher,
      ...
    }:
    let
      checked = import ../verification.nix;
      nativePackage = import ../native-package.nix;
      source =
        origin:
        checked.resource {
          root = ../..;
          inherit origin;
        };
      evaluation = import ../native-context.nix;
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
      };
      smokeRuntime = testsDeriv.loom-smoke;
      smokeScript = source "scripts/run-smoke.sh";
      smokePreflight = checked.check {
        resources = [ smokeScript ];
        inherit evaluation;
        build =
          paths:
          pkgs.writeShellApplication {
            name = "smoke";
            runtimeInputs = [ pkgs.nix ];
            text = ''
              exec ${pkgs.bash}/bin/bash ${builtins.head paths} /dev/fuse /dev/net/tun .#smoke-runtime "$@"
            '';
          };
      };
      smokeApp = if pkgs.stdenv.hostPlatform.isLinux then smokePreflight else smokeRuntime;
      testCompilerInputs = [
        pkgs.stdenv.cc
        pkgs.sccache
      ];
      testRuntimeInputs = [
        pkgs.coreutils
        pkgs.cargo-nextest
        pkgs.dolt
        pkgs.git
        pkgs.nix
        pkgs.python3
        pkgs.beads
        pkgs.openssh
        loom.hookRunner
        wrixLib.skipIfMissing
        loom.bin
        loom.toolchain
      ];
      mkTestApp =
        {
          name,
          text,
          runtimeInputs ? testRuntimeInputs,
          evaluation ? [ ],
        }:
        checked.check {
          resources = [ ];
          evaluation = (import ../native-context.nix) ++ evaluation;
          build =
            _:
            pkgs.writeShellApplication {
              inherit name;
              text = builtins.readFile ../test-cargo-context.sh + "\n" + text;
              runtimeInputs = runtimeInputs ++ testCompilerInputs;
              runtimeEnv = {
                LOOM_TEST_TOOLCHAIN = loom.toolchain;
                LOOM_CARGO_PATCHES = loom.cargoConfig;
                LOOM_PREK_BIN = "${loom.hookRunner}/bin/prek";
                LOOM_ISSUES_BD_BIN = "${pkgs.beads}/bin/bd";
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
        text = builtins.readFile fullScript.path;
        evaluation = [ fullScript ];
      };
      requiredTestAppConfig = {
        name = "test-required";
        text = builtins.readFile requiredScript.path;
        evaluation = [ requiredScript ];
      };
      testApp = mkTestApp testAppConfig;
      requiredTestApp = mkTestApp requiredTestAppConfig;
      decisionAdmissionApp = mkTestApp {
        name = "test-decision-admission";
        runtimeInputs = testRuntimeInputs ++ [ pkgs.beads ];
        text = ''
          bash scripts/configure-cargo-patches.sh
          cargo nextest run -p loom-workflow --no-fail-fast --ignore-default-filter \
            -E 'test(decision_wait) | test(native_controller_counts_one_noop_source_and_five_distinct_decisions) | test(integrity_extension_requires_explicit_reusable_source_readmission)'
        '';
      };
      nativeUnitDriver = loom.craneLib.mkCargoDerivation {
        pname = "loom-native-unit-driver";
        version = "0.1.0";
        doInstallCargoArtifacts = false;
        LOOM_PREK_BIN = "${loom.hookRunner}/bin/prek";
        src = loom.stagedSrc;
        inherit (loom) cargoArtifacts cargoVendorDir;
        nativeBuildInputs = [ pkgs.git ];
        buildPhaseCargoCommand = ''
          cargo build --release -p loom-gate --example native-isolation --example native-rust --example native-reported
          cargo build --release -p loom --bin loom
        '';
        installPhaseCommand = ''
          install -Dm755 target/release/examples/native-isolation "$out/bin/native-isolation"
          install -Dm755 target/release/examples/native-rust "$out/bin/native-rust"
          install -Dm755 target/release/examples/native-reported "$out/bin/native-reported"
          install -Dm755 target/release/loom "$out/bin/loom"
        '';
        doCheck = false;
      };
      nativeRustFixture = import ../tests/native-rust-fixture.nix {
        inherit pkgs;
        inherit (inputs) crane;
        inherit (loom) toolchain;
        nextest = pkgs.cargo-nextest;
      };
      nativeSmokeFixture = import ../tests/native-smoke-fixture.nix { inherit pkgs; };
      nativeUnitsApp = mkTestApp {
        name = "test-native-units";
        runtimeInputs = [
          pkgs.coreutils
          pkgs.git
          pkgs.nix
          pkgs.bash
          pkgs.python3
          loom.toolchain
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
          python3 "$fixture/tests/verify/native-resources.py" "$fixture"
          if ${nativeUnitDriver}/bin/native-isolation > "$fixture/native-observed.jsonl"; then
            cat "$fixture/native-observed.jsonl"
            ${nativeUnitDriver}/bin/native-reported ${nativeUnitDriver}/bin/loom
            ${nativeUnitDriver}/bin/native-rust ${nativeRustFixture} ${nativeUnitDriver}/bin/loom
          else
            status=$?
            cat "$fixture/native-observed.jsonl" >&2
            exit "$status"
          fi
        '';
      };
      fixtureNix = pkgs.writeShellScriptBin "nix" ''
        case "$*" in
          "fmt -- --fail-on-change" | "build" | "flake check --no-warn-dirty" | "run .#test-native-units") exit 0 ;;
          "flake metadata "*)
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
        pkgs.python3
        pkgs.cargo-nextest
        pkgs.git
        fixtureNix
        loom.toolchain
      ];
      fixtureTestApp = mkTestApp (testAppConfig // { runtimeInputs = fixtureRuntimeInputs; });
      fixtureRequiredTestApp = mkTestApp (
        requiredTestAppConfig // { runtimeInputs = fixtureRuntimeInputs; }
      );
      fixtureProfile = pkgs.writeText "bootstrap-fixture-profile.json" ''{"profile":{"mounts":[]}}'';
      fixtureImage = pkgs.writeText "bootstrap-fixture-image.json" "{}";
      fixtureManifest = pkgs.writeText "bootstrap-fixture-manifest.json" (
        builtins.toJSON {
          rust.direct = {
            ref = "bootstrap-fixture";
            source = "${fixtureImage}";
            source_kind = "nix-descriptor";
            launcher = "${wrixLauncher}/bin/wrix";
            profile_config = "${fixtureProfile}";
            native_preflight = "rust";
          };
        }
      );
      bootstrapScript = source "tests/sandbox/profile-bootstrap.sh";
      profileBootstrapApp = checked.check {
        resources = [ bootstrapScript ];
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
            ];
            runtimeEnv.WRIX_PREK_HOOKS = wrixLib.prekHooks;
            runtimeEnv.LOOM_WRIX_BIN = "${wrixLauncher}/bin/wrix";
            text = ''
              [[ $# == 0 ]] || exit 64
              exec ${pkgs.bash}/bin/bash ${builtins.head paths} \
                ${loom.bin}/bin/loom ${wrixLauncher}/bin/wrix ${fixtureManifest} \
                ${pkgs.bash} ${pkgs.coreutils} ${pkgs.stdenv.hostPlatform.system}
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
      packages = {
        smoke-runtime = nativePackage smokeRuntime;
      }
      // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        native-rust-fixture = nativePackage nativeRustFixture;
        native-smoke-fixture = nativePackage nativeSmokeFixture;
      };
      checks.test-app-hook-runner = checked.check {
        resources = map source [
          "tests/agent/test-app-hook-runner.sh"
          "tests/verify/native-resources.py"
        ];
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
              bash ${builtins.elemAt paths 0} \
                ${fixtureRequiredTestApp}/bin/test-required ${fixtureTestApp}/bin/test \
                ${loom.hookRunner}/bin/prek ${pkgs.beads}/bin/bd ${builtins.elemAt paths 1} ${pkgs.sccache}/bin/sccache ${pkgs.nix}/bin/nix
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
        smoke = app smokeApp "smoke" "Run the Loom mock-agent assembly smoke.";
        fuzz-loom = app fuzzApp "fuzz-loom" "Run an on-demand fuzz target.";
      };
    };
}

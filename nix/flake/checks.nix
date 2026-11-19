{ inputs, ... }:
{
  perSystem =
    {
      pkgs,
      config,
      loom,
      system,
      wrixLib,
      wrixLauncher,
      ...
    }:
    let
      inherit (pkgs.lib) optionalString;
      inherit (loom)
        bin
        cargoArtifacts
        craneLib
        stagedSrc
        ;
      checked = import ../verification.nix;
      resource =
        origin:
        checked.resource {
          root = ../..;
          inherit origin;
        };
      evaluation = import ../native-context.nix;
      verificationTool = import ../verification-tool.nix { inherit pkgs; };
      toolCheck =
        name: resources: inputs: script:
        scriptCheck name (resources ++ verificationTool.loom.sources) (
          inputs ++ [ verificationTool ]
        ) script;
      check =
        resources: build:
        checked.check {
          inherit resources evaluation build;
          runtime = "native-process";
        };
      scriptCheck =
        name: resources: nativeBuildInputs: script:
        check resources (
          paths:
          pkgs.runCommand name { nativeBuildInputs = map pkgs.lib.getBin nativeBuildInputs; } ''
            set -euo pipefail
            ${script paths}
            touch "$out"
          ''
        );
      testsDeriv = import ../../tests/default.nix {
        inherit pkgs wrixLauncher;
        loomPackage = loom;
      };
      checkedNativeCargo = import ../native-cargo.nix {
        inherit pkgs;
        config = loom.cargoConfig;
        toolchain = loom.toolchain;
      };
      bootstrapConfig =
        (pkgs.writeText "bootstrap-cargo-patches.toml" ''
          paths = []
          [env]
          LOOM_PREK_BIN = { value = "${loom.hookRunner}/bin/prek", force = true }
          LOOM_ISSUES_BD_BIN = { value = "${pkgs.beads}/bin/bd", force = true }
          LOOM_TEST_DOLT_BIN = { value = "${loom.dolt}/bin/dolt", force = true }
          LOOM_NATIVE_NIX_BIN = { value = "${pkgs.nix}/bin/nix", force = true }
          ${optionalString pkgs.stdenv.hostPlatform.isLinux ''
            LOOM_NATIVE_FLOCK_BIN = { value = "${pkgs.util-linux}/bin/flock", force = true }
            LOOM_NATIVE_SHELL_BIN = { value = "${pkgs.bash}/bin/bash", force = true }
            LOOM_NATIVE_STAT_BIN = { value = "${pkgs.coreutils}/bin/stat", force = true }
          ''}
        '').overrideAttrs
          {
            passthru = {
              nextestVersion = "0.23.0";
              nextestChecksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            };
          };
      bootstrapCargo = import ../native-cargo.nix {
        inherit pkgs;
        config = bootstrapConfig;
        toolchain = pkgs.writeShellScriptBin "cargo" ''
          set -euo pipefail
          printf '%s\n' "$@"
        '';
      };
      fakeLoomBin = pkgs.writeShellApplication {
        name = "loom";
        text = ''
          if [[ "''${1:-}" == --launch-inputs ]]; then
            printf 'git=%s\nraw=%s\n' "''${LOOM_WRIX_BIN:-}" "''${LOOM_WRIX_SPAWN_BIN:-}"
          elif [[ "''${1:-}" == --profile-environment ]]; then
            env | grep -E '^LOOM_(PROFILE_CATALOGUE|PROFILES_MANIFEST|PROVISIONED_PROFILES_MANIFEST|NATIVE_PREFLIGHT_MANIFEST)=' || true
          else
            printf 'loom 0.0.0\n'
          fi
        '';
      };
      fakeUnprofiledWrix = pkgs.writeShellScriptBin "wrix" ''printf 'unprofiled wrix %s\n' "$*"'';
      fakeProfiledWrix =
        (pkgs.writeShellScriptBin "wrix" ''
          exec ${fakeUnprofiledWrix}/bin/wrix --profile-config /nix/store/fake-profile.json "$@"
        '').overrideAttrs
          (old: {
            passthru = (old.passthru or { }) // {
              launcher = fakeUnprofiledWrix;
            };
          });
      fakeLoomWrix = (import ../lib.nix).mkLoomBin {
        # Exercise launcher wiring without constructing unrelated container backends.
        pkgs = pkgs // {
          podman = pkgs.writeShellScriptBin "podman" "exit 97";
          skopeo = pkgs.writeShellScriptBin "skopeo" "exit 97";
        };
        loomBuild.bin = fakeLoomBin;
        wrixLauncher = fakeProfiledWrix;
      };
      fakeSmokeNix = pkgs.writeShellScriptBin "nix" ''
        touch "$SMOKE_NIX_CALLED"
        exit 1
      '';
      darwinSmokeText =
        (import ../../tests/loom/default.nix {
          pkgs = {
            inherit (pkgs) lib;
            stdenv.hostPlatform.isLinux = false;
            writeShellApplication = { text, ... }: { inherit text; };
          };
          loomPackage = throw "unavailable smoke must not force a native build";
        }).loom-smoke.text;
      llmCompile =
        flags:
        check loom.testResources (
          _:
          craneLib.mkCargoDerivation {
            pname = "loom-llm-feature-check";
            version = "0.0.0";
            src = stagedSrc;
            inherit cargoArtifacts;
            inherit (loom) cargoVendorDir;
            doCheck = false;
            doInstallCargoArtifacts = false;
            preBuild = "export HOME=$(mktemp -d)";
            buildPhaseCargoCommand = ''cargo check --offline --locked --profile "$CARGO_PROFILE" -p loom-llm ${flags}'';
            installPhaseCommand = "touch $out";
          }
        );
      llmCompatCases = map (name: "client::openai_compat::tests::${name}") [
        "openai_compat_multimodal_returns_unsupported_without_network"
        "openai_compat_client_sends_chat_completions_shape_to_configured_url"
        "openai_compat_client_rejects_non_compat_modelids"
        "openai_compat_http_contract_covers_status_classes"
      ];
      llmCompatArchive = craneLib.mkCargoDerivation {
        pname = "loom-llm-compat-archive";
        version = "0.0.0";
        src = loom.compilerSource.path;
        cargoArtifacts = loom.testCargoArtifacts;
        CARGO_PROFILE = "dev";
        inherit (loom) cargoVendorDir;
        nativeBuildInputs = [
          loom.writableWorkspaceArtifacts
        ]
        ++ map pkgs.lib.getBin [
          pkgs.cargo-nextest
          pkgs.bash
          pkgs.gnutar
          pkgs.zstd
          pkgs.jq
        ];
        doCheck = false;
        doInstallCargoArtifacts = false;
        NIX_NO_SELF_RPATH = 1;
        buildPhaseCargoCommand = ''
          export HOME=$(mktemp -d)
          export CARGO_TARGET_DIR="$PWD/target"
          cargo nextest archive --offline --locked --cargo-profile "$CARGO_PROFILE" \
            --manifest-path ${loom.compilerSource.path}/Cargo.toml \
            -p loom-llm --features openai-compat --lib --archive-file "$TMPDIR/llm-compat.tar.zst"
          test ! -e ${loom.compilerSource.path}/target
        '';
        installPhaseCommand = ''
          mkdir -p "$out"
          bash ${../normalize-nextest-archive.sh} "$TMPDIR/llm-compat.tar.zst" "$out/archive.tar.zst"
        '';
      };
      llmCompatProgram = pkgs.writeShellApplication {
        name = "llm-compat-report";
        runtimeInputs = [
          pkgs.bash
          pkgs.cargo-nextest
          pkgs.coreutils
          pkgs.findutils
        ];
        text = ''
          export HOME
          HOME=$(mktemp -d)
          mkdir -p "$HOME/tests"
          export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
          export NIX_SSL_CERT_FILE="$SSL_CERT_FILE"
          export NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1
          export LOOM_TEST_SOURCE_ROOT=${stagedSrc}
          exec cargo-nextest nextest run --archive-file ${llmCompatArchive}/archive.tar.zst \
            --extract-to "$HOME/tests" --workspace-remap ${stagedSrc} \
            --ignore-default-filter --test-threads 2 --no-fail-fast \
            -E '${builtins.concatStringsSep " | " (map (name: "test(=${name})") llmCompatCases)}' \
            --message-format libtest-json-plus
        '';
      };
      consumerSource = checked.resource {
        root = ../..;
        name = "loom-consumer-source";
        filter =
          path: type:
          let
            relative = pkgs.lib.removePrefix "${toString ../..}/" path;
            extraOrigins = map (source: source.origin) (
              builtins.filter (source: source.origin != ".") loom.testResources
            );
          in
          type == "directory"
          || loom.workspaceSource.includes path type
          || builtins.any (
            origin: relative == origin || pkgs.lib.hasPrefix "${origin}/" relative
          ) extraOrigins;
      };
      checks = {
        native-compiler-linking = check [ ] (
          _:
          pkgs.runCommand "native-compiler-linking"
            {
              nativeBuildInputs = [
                loom.toolchain
                pkgs.stdenv.cc
              ]
              ++ loom.testCompilerHooks;
            }
            ''
              printf 'int main(void) { return 0; }\n' > linker.c
              "$CC" linker.c -o linker-probe
              printf 'fn main() {}\n' > compiler.rs
              rustc compiler.rs -o compiler-probe
              ./linker-probe
              ./compiler-probe
              touch "$out"
            ''
        );
        confined-runtime = loom.confinedTests;
        kernel-free-surface = checked.check {
          runtime = "native-process";
          resources = [ ];
          evaluation = (import ../native-context.nix) ++ loom.testResources;
          build =
            _:
            pkgs.runCommand "kernel-free-surface" { } (
              let
                native =
                  value:
                  (value.loom.runtime or "nix") == "native-process"
                  || ((value.loom.runtime or "nix") == "native-store" && (value.loom.cargo or null) == "test");
              in
              assert builtins.all native (builtins.attrValues config.packages);
              assert builtins.all native (builtins.attrValues config.checks);
              assert builtins.all (value: (value.loom.purpose or "check") == "check") (
                builtins.attrValues config.checks
              );
              assert builtins.all (app: native app.meta.loom.definition) (builtins.attrValues config.apps);
              assert !(config.packages ? smoke-runtime);
              assert config.legacyPackages ? kernel-smoke-runtime;
              ''touch "$out"''
            );
        };
        agent-landing =
          scriptCheck "agent-landing"
            [
              (resource "tests/verify/agent-landing.sh")
              (resource "nix/private-fixture-tests.sh")
              (resource "bin/pre-push-checks")
            ]
            [
              pkgs.bash
              pkgs.git
              pkgs.openssh
              pkgs.coreutils
              loom.hookRunner
              loom.bin
            ]
            (paths: ''
              bash ${builtins.elemAt paths 1} --test-process bash ${builtins.head paths} \
                ${builtins.elemAt paths 2}
            '');
        private-fixture-nix-policy =
          scriptCheck "private-fixture-nix-policy"
            [
              (resource "nix/private-fixture-tests.sh")
              (resource "tests/agent/private-fixture-nix-policy.sh")
            ]
            [
              pkgs.bash
              pkgs.coreutils
              pkgs.nix
              pkgs.git
            ]
            (
              paths:
              let
                seed = pkgs.writeText "private-sdk-policy-seed" "declared SDK bytes\n";
                registration = pkgs.closureInfo { rootPaths = [ seed ]; };
              in
              ''
                LOOM_POLICY_REGISTRATION=${registration}/registration LOOM_POLICY_SEED=${seed} \
                  bash ${builtins.elemAt paths 1} ${builtins.elemAt paths 0}
              ''
            );
        wrix-lean-layer-plan = toolCheck "wrix-lean-layer-plan" [ ] [ ] (_: ''
          loom-verification layer-conformance \
            ${pkgs.path}/pkgs/by-name/fl/flatten-references-graph/src/flatten_references_graph/popularity_contest.py
          ${verificationTool}/libexec/loom-verification-tests layer::tests
        '');
        nextest-archive-reproducible = check [ (resource "nix/normalize-nextest-archive.sh") ] (
          paths:
          import ../tests/normalize-nextest-archive.nix {
            inherit pkgs;
            normalizer = builtins.head paths;
          }
        );
        system-verification-report = check [ ] (
          _: import ../tests/system-verification-report.nix { inherit pkgs system; }
        );
        profile-catalogue = checked.check {
          runtime = "native-process";
          resources = [ ];
          evaluation = evaluation ++ loom.testResources;
          build = _: import ../tests/profile-catalogue.nix { inherit pkgs wrixLib; };
        };
        loom-consumer-api = checked.check {
          runtime = "native-process";
          resources = [
            consumerSource
          ]
          ++ verificationTool.loom.sources
          ++ [
          ];
          evaluation = evaluation ++ [ (resource "tests/verify/consumer-api.nix") ];
          build =
            paths:
            import ../../tests/verify/consumer-api.nix {
              inherit pkgs;
              inherit (inputs) crane;
              src = builtins.head paths;
              inherit verificationTool;
            };
        };
        wrix-consumer-evaluation = checked.check {
          runtime = "native-process";
          resources = [ ];
          evaluation = evaluation ++ loom.testResources;
          build =
            _:
            pkgs.writeText "wrix-consumer-evaluation.json" (
              builtins.unsafeDiscardStringContext (
                builtins.toJSON {
                  devShell = config.devShells.default.drvPath;
                  serviceImage = wrixLib.serviceImage.drvPath;
                }
              )
            );
        };
        smoke-unavailable = scriptCheck "smoke-unavailable" [ ] [ pkgs.bash pkgs.gnugrep ] (_: ''
          status=0
          bash -euo pipefail ${pkgs.writeText "unavailable-smoke.sh" darwinSmokeText} >"$TMPDIR/smoke.out" 2>&1 || status=$?
          [[ "$status" == 69 ]]
          grep -Fq 'container smoke not available on Darwin' "$TMPDIR/smoke.out"
        '');
        native-check-discovery = check [ ] (
          _:
          pkgs.writeText "native-check-discovery.json" (
            builtins.toJSON (import ../../tests/verify/native-check-discovery.nix { lib = inputs.nixpkgs.lib; })
          )
        );
        catalogue-reuse = import ../tests/catalogue-reuse.nix { inherit pkgs loom; };

        workspace-build-identity = checked.check {
          runtime = "native-process";
          resources = [ ];
          evaluation = evaluation ++ loom.testResources;
          build =
            _:
            import ../tests/workspace-identity.nix {
              inherit pkgs loom;
              inherit (inputs) crane fenix;
              src = ../..;
            };
        };
        workspace-source-includes-git-policy-scripts =
          scriptCheck "workspace-source-includes-git-policy-scripts"
            ([ (resource "crates/loom-test-support/src/git_policy") ] ++ loom.testResources)
            [ ]
            (paths: ''
              for src in ${bin.src} ${stagedSrc}; do
                for script in wrix.sh sign.sh; do
                  cmp "${builtins.head paths}/$script" "$src/crates/loom-test-support/src/git_policy/$script"
                done
              done
            '');
        workspace-source-includes-nix-support =
          scriptCheck "workspace-source-includes-nix-support"
            (
              [
                (resource "nix")
                (resource "bin/loom")
              ]
              ++ loom.testResources
            )
            [ pkgs.diffutils ]
            (paths: ''
              diff --recursive ${builtins.elemAt paths 0} ${stagedSrc}/nix
              cmp ${builtins.elemAt paths 1} ${stagedSrc}/bin/loom
            '');
        nextest-cargo-home-isolation =
          scriptCheck "nextest-cargo-home-isolation"
            [ (resource "tests/agent/nextest-home.sh") ]
            [ loom.toolchain pkgs.stdenv.cc ]
            (paths: ''
              bash ${builtins.head paths} ${pkgs.writeText "nextest-pre-unpack.sh" pkgs.cargo-nextest.preUnpack}
              bash ${builtins.head paths} ${
                pkgs.writeText "prek-pre-unpack.sh" (import ../prek.nix { inherit pkgs; }).preUnpack
              }
            '');
        acceptance-storage-report = toolCheck "acceptance-storage-report" [ ] [ pkgs.coreutils ] (_: ''
          ${verificationTool}/libexec/loom-verification-tests storage::tests
          LOOM_VERIFICATION_BIN=${verificationTool}/bin/loom-verification \
            ${verificationTool}/libexec/loom-verification-cli-tests cli::storage_
        '');
        native-smoke-beads = check [ ] (_: import ../tests/native-smoke-beads.nix { inherit pkgs; });
        test-cargo-context =
          toolCheck "test-cargo-context"
            (map resource ([
              "nix/test-cargo-context.sh"
              "tests/fixtures/test-cargo-context.sh"
              "nix/test-scheduling.sh"
              "tests/fixtures/test-scheduling.sh"
              "nix/private-fixture-tests.sh"
              "Cargo.toml"
              "nix/writable-cargo-artifacts.sh"
            ]))
            (
              [
                pkgs.bash
                pkgs.coreutils
                pkgs.git
                loom.toolchain
                pkgs.stdenv.cc
                pkgs.binutils
              ]
              ++ loom.testCompilerHooks
            )
            (paths: ''
              bash ${builtins.elemAt paths 4} --test-process bash ${builtins.elemAt paths 1} ${builtins.head paths} ${builtins.elemAt paths 2}
              bash ${builtins.elemAt paths 3} ${builtins.elemAt paths 2}
              loom-verification cargo-footprint ${builtins.elemAt paths 5}
              loom-verification writable-artifacts ${builtins.elemAt paths 6} ${loom.toolchain}/bin/rustc
              ${optionalString pkgs.stdenv.hostPlatform.isLinux ''
                loom-verification debug-compression ${builtins.head loom.testCompilerHooks}/nix-support/setup-hook ${loom.toolchain}/bin/rustc
              ''}
            '');
        verification-preparation =
          toolCheck "verification-preparation"
            [ (resource "nix/private-fixture-tests.sh") ]
            [
              (import ../tests/private-nix.nix { inherit pkgs; })
              pkgs.gnugrep
              pkgs.coreutils
            ]
            (
              paths:
              let
                ready = pkgs.closureInfo { rootPaths = [ (pkgs.writeText "sdk-seed" "pinned SDK fixture") ]; };
                cold = pkgs.writeTextDir "store-paths" ''
                  /nix/store/00000000000000000000000000000000-missing-sdk-a
                  /nix/store/00000000000000000000000000000000-missing-sdk-b
                '';
                fixtureRegistration = pkgs.closureInfo {
                  rootPaths = [
                    ready
                    cold
                  ];
                };
              in
              ''
                bash ${builtins.head paths} --test-process ${verificationTool}/libexec/loom-verification-tests preparation::tests
                LOOM_VERIFICATION_BIN=${verificationTool}/bin/loom-verification \
                  bash ${builtins.head paths} --test-process \
                  ${verificationTool}/libexec/loom-verification-cli-tests cli::preparation_
                export HOME="$TMPDIR/sdk-home" NIX_REMOTE=local
                export NIX_STATE_DIR="$TMPDIR/sdk-nix-state" NIX_LOG_DIR="$TMPDIR/sdk-nix-log" NIX_CONF_DIR="$TMPDIR/sdk-nix-conf"
                mkdir -p "$HOME" "$NIX_STATE_DIR" "$NIX_LOG_DIR" "$NIX_CONF_DIR"
                export NIX_CONFIG=$'experimental-features = nix-command flakes\nbuild-users-group =\nrequire-sigs = true\nsandbox = true\nsandbox-fallback = false\nsubstituters =\nbuilders ='
                nix-store --load-db < ${fixtureRegistration}/registration
                timeout --kill-after=5s 65s loom-verification prepare ${builtins.unsafeDiscardStringContext ready.drvPath} ${ready} check
                status=0
                timeout --kill-after=5s 65s loom-verification prepare ${builtins.unsafeDiscardStringContext cold.drvPath} ${cold} check > missing.log 2>&1 || status=$?
                [[ "$status" == 78 ]]
                grep -Fq 'missing-sdk-a' missing.log
                grep -Fq 'missing-sdk-b' missing.log
              ''
            );
        native-stage-integrity =
          check
            (
              loom.testResources
              ++ verificationTool.loom.sources
              ++ [
                (resource "bin/pre-push-checks")
                (resource "tests/agent/skopeo-owned-temp.sh")
              ]
            )
            (
              _:
              assert config.packages.quint-tools.loom.runtime == "native-process";
              assert !loom.verificationBuild.doInstallCargoArtifacts;
              assert loom.verificationBuild.CARGO_PROFILE == "dev";
              assert loom.verificationBuild.LOOM_TEST_DOLT_BIN == "${loom.dolt}/bin/dolt";
              assert loom.testCompilation.LOOM_NATIVE_SHELL_BIN == pkgs.runtimeShell;
              assert loom.verificationBuild.LOOM_NATIVE_SHELL_BIN == pkgs.runtimeShell;
              assert loom.verificationBuild.cargoArtifacts.drvPath == loom.testCompilation.drvPath;
              assert loom.verificationBuild.src == loom.nextest.src;
              assert !loom.clippy.doInstallCargoArtifacts;
              assert !loom.clippyAllTargets.doInstallCargoArtifacts;
              assert loom.clippy.cargoArtifacts == loom.cargoArtifacts;
              assert loom.clippyAllTargets.cargoArtifacts == loom.cargoArtifacts;
              assert pkgs.lib.all (
                input:
                !(builtins.elem (input.pname or "") [
                  "jq"
                  "openssh"
                ])
                || (input.outputSpecified or false)
              ) loom.nextest.nativeBuildInputs;
              assert loom.nextest.CARGO_PROFILE == "dev";
              assert loom.testCompilation.drvPath == loom.nextest.loom.catalogue;
              assert !loom.testCompilation.doInstallCargoArtifacts;
              assert loom.testCompilation.CARGO_PROFILE == "dev";
              assert llmCompatArchive.cargoArtifacts.drvPath == loom.testCargoArtifacts.drvPath;
              assert llmCompatArchive.CARGO_PROFILE == "dev";
              assert pkgs.lib.hasPrefix "/nix/store/" loom.bin.LOOM_BUILTIN_SOURCE_ROOT;
              assert pkgs.lib.hasSuffix "/crates/loom-skill" loom.bin.LOOM_BUILTIN_SOURCE_ROOT;
              assert (loom.bin.CARGO_PROFILE or "release") == "release";
              assert (loom.clippy.CARGO_PROFILE or "release") == "release";
              assert (loom.clippyAllTargets.CARGO_PROFILE or "release") == "release";
              assert loom.clippy.loom.cargo == "lint";
              assert loom.clippyAllTargets.loom.cargo == "lint-all-targets";
              assert !(builtins.hasAttr "loom-tests" config.checks);
              assert !(builtins.hasAttr "fuzz-loom" config.checks);
              assert !(builtins.hasAttr "loomTests" config.checks);
              assert
                !pkgs.stdenv.hostPlatform.isDarwin
                || builtins.all (name: config.packages.${name}.drvPath == loom.nextest.drvPath) [
                  "verification-system-ci-shell"
                  "verification-system-decision-admission"
                  "verification-system-profile-bootstrap"
                  "verification-system-smoke"
                ];
              assert
                !pkgs.stdenv.hostPlatform.isDarwin
                ||
                  builtins.attrNames (
                    builtins.fromJSON (
                      builtins.unsafeDiscardStringContext (builtins.readFile config.legacyPackages.native-system-programs)
                    )
                  ) == [
                    "ci-shell"
                    "decision-admission"
                    "profile-bootstrap"
                    "smoke"
                  ];
              assert config.packages.loom-tests.drvPath == loom.nextest.drvPath;
              assert config.packages.loom-tests.loom.cargo == "test";
              assert config.packages.loom-tests.loom.catalogue == loom.nextest.loom.catalogue;
              assert !loom.nextest.loom.reporter.doInstallCargoArtifacts;
              assert !loom.nextest.loom.reporter.cargoArtifacts.doInstallCargoArtifacts;
              assert
                loom.nextest.loom.reporter.prePatch
                == ''postPatchHooks=("''${postPatchHooks[@]/inheritCargoArtifacts/}")'';
              pkgs.runCommand "native-staged-source-integrity"
                {
                  nativeBuildInputs = [
                    pkgs.jq
                    pkgs.gzip
                  ];
                }
                ''
                  ${optionalString pkgs.stdenv.hostPlatform.isDarwin ''
                    bash ${(resource "tests/agent/skopeo-owned-temp.sh").path} \
                      ${import ../host-skopeo.nix { inherit pkgs; }}/bin/skopeo ${pkgs.skopeo}/bin/skopeo
                    ${pkgs.jq}/bin/jq -e '[."rust-suites"[].testcases["native_system::native_system_scenarios_execute_actual_programs_in_canonical_store"] | select(. != null)] | length == 1 and .[0].ignored == false' ${loom.testCompilation}/default.json > /dev/null
                  ''}
                  test -x ${stagedSrc}/bin/pre-push-checks
                  cmp ${stagedSrc}/bin/pre-push-checks ${(resource "bin/pre-push-checks").path}
                  cmp ${stagedSrc}/crates/loom-agent/src/pi/parser/terminal_provider_error.jsonl \
                    ${(resource "crates/loom-agent/src/pi/parser/terminal_provider_error.jsonl").path}
                  ${verificationTool}/libexec/loom-verification-tests native::tests
                  LOOM_VERIFICATION_BIN=${verificationTool}/bin/loom-verification \
                    ${verificationTool}/libexec/loom-verification-cli-tests cli::native_
                  touch "$out"
                ''
            );
        llm-compat-compile = llmCompile "--features openai-compat";
        llm-native-compile = llmCompile "--no-default-features";
        llm-compat-report = checked.reported {
          runtime = "native-process";
          inherit system evaluation;
          name = "loom-llm-compat-report";
          bash = pkgs.bash;
          coreutils = pkgs.coreutils;
          resources = loom.testResources;
          program = "${llmCompatProgram}/bin/llm-compat-report";
          parser = "libtest-json";
          targets = map (name: "loom-llm::loom_llm$" + name) llmCompatCases;
        };
        nextest-filtering-syntax =
          scriptCheck "nextest-filtering-syntax" [ loom.compilerSource ] [ pkgs.cargo-nextest ]
            (_: ''
              export HOME=$(mktemp -d)
              cargo-nextest nextest run \
                --cargo-metadata ${loom.testCompilation}/cargo.json \
                --binaries-metadata ${loom.testCompilation}/binaries.json \
                --workspace-remap ${loom.compilerSource.path} \
                --target-dir-remap ${loom.testCompilation}/target \
                --ignore-default-filter --no-fail-fast \
                -E 'package(=loom-gate) & binary(=integration) & test(/^nextest_filtering::/)'
            '');
        native-cargo-bootstrap =
          scriptCheck "native-cargo-bootstrap" loom.testResources
            [ pkgs.git (pkgs.lib.getBin pkgs.nix) pkgs.coreutils ]
            (_: ''
              cp -r ${stagedSrc} source
              chmod -R u+w source
              cd source
              git init -q
              bash tests/agent/native-cargo.sh ${bootstrapCargo}/bin/cargo \
                ${bootstrapConfig} ${bootstrapConfig.nextestVersion} ${bootstrapConfig.nextestChecksum} ${loom.hookRunner}/bin/prek
            '');
        native-cargo-patches = check loom.testResources (
          _:
          craneLib.mkCargoDerivation {
            pname = "native-cargo-patches";
            version = "0.0.0";
            src = stagedSrc;
            cargoArtifacts = loom.testCargoArtifacts;
            CARGO_PROFILE = "dev";
            cargoVendorDir = (inputs.crane.mkLib pkgs).vendorCargoDeps {
              cargoLock = ../../Cargo.lock;
              cargoToml = ../../Cargo.toml;
            };
            nativeBuildInputs = [ (pkgs.lib.getBin pkgs.nix) ];
            # This control deliberately switches vendor sources at unchanged crate IDs.
            doNotLinkInheritedArtifacts = true;
            doCheck = true;
            doInstallCargoArtifacts = false;
            preBuild = ''
              export HOME=$(mktemp -d)
              cp Cargo.lock "$TMPDIR/original-Cargo.lock"
              if cargo test --locked --profile "$CARGO_PROFILE" -p loom-gate --test integration nextest_filtering::nextest_filter_grammar_errors_retain_upstream_diagnostics > "$TMPDIR/unpatched.log" 2>&1; then
                echo "unpatched parser unexpectedly passed the strict regression" >&2
                exit 1
              fi
              if ! grep -F 'test nextest_filtering::nextest_filter_grammar_errors_retain_upstream_diagnostics ... FAILED' "$TMPDIR/unpatched.log"; then
                cat "$TMPDIR/unpatched.log" >&2
                exit 1
              fi
              ${checkedNativeCargo}/bin/cargo --version
              test ! -e .cargo/nix-patches.toml
              ${checkedNativeCargo}/bin/cargo test --locked --profile "$CARGO_PROFILE" -p loom-gate --test integration nextest_filtering::nextest_filter_grammar_errors_retain_upstream_diagnostics
              test -L .cargo/nix-patches.toml
            '';
            buildPhaseCargoCommand = ''
              cargo test --locked --profile "$CARGO_PROFILE" -p loom-gate --test integration nextest_filtering::
              cargo test --locked --profile "$CARGO_PROFILE" -p loom-gate --test integration strategy::
            '';
            checkPhaseCargoCommand = ''cmp Cargo.lock "$TMPDIR/original-Cargo.lock"'';
            installPhaseCommand = "touch $out";
          }
        );
        loom-gate-check = check loom.testResources (
          _:
          pkgs.runCommand "loom-source-walks"
            {
              nativeBuildInputs = map pkgs.lib.getBin [
                pkgs.git
                pkgs.nix
                pkgs.cacert
                pkgs.cargo-nextest
                pkgs.jq
              ];
            }
            ''
              export HOME=$(mktemp -d)
              export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
              export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
              cp -r ${stagedSrc} source
              chmod -R u+w source
              cd source
              test ! -e .git && test ! -e target && test ! -e .cargo-home
              git init -q -b main
              printf '/target/\n/.cargo-home/\n' >> .git/info/exclude
              git add --all
              GIT_AUTHOR_DATE="@1 +0000" GIT_COMMITTER_DATE="@1 +0000" \
                git -c user.name="Loom native source fixture" -c user.email="native-source-fixture@example.invalid" \
                  -c commit.gpgsign=false commit -q -m "Seal staged verification source"
              bash ${../private-fixture-tests.sh} ${bin}/bin/loom-walk --from-specs
              touch "$out"
            ''
        );
        worker-native-preflight =
          scriptCheck "worker-native-preflight"
            (map resource [
              "tests/sandbox/native-preflight-test.sh"
              "scripts/worker-native-preflight.sh"
            ])
            [ pkgs.bash pkgs.coreutils pkgs.gnugrep ]
            (paths: ''
              bash ${builtins.elemAt paths 0} ${builtins.elemAt paths 1}
            '');
        test-app-ignores-host-git-signing = testsDeriv.test-app-ignores-host-git-signing;
        smoke-preflight-skips-runtime-build =
          scriptCheck "smoke-preflight-skips-runtime-build" [ (resource "scripts/run-smoke.sh") ] [ ]
            (paths: ''
              export PATH="${fakeSmokeNix}/bin:$PATH"
              export SMOKE_NIX_CALLED="$TMPDIR/nix-called"
              status=0
              bash ${builtins.head paths} "$TMPDIR/missing-fuse" "$TMPDIR/missing-tun" .#smoke-runtime > "$TMPDIR/output" 2>&1 || status=$?
              [[ "$status" == 77 ]]
              [[ ! -e "$SMOKE_NIX_CALLED" ]]
              grep -F 'skip: container runtime devices' "$TMPDIR/output"
            '');
        loom-wrapper-preserves-spaced-output-path =
          scriptCheck "loom-wrapper-preserves-spaced-output-path" [ ] [ pkgs.makeWrapper ]
            (_: ''
              (
                out="$TMPDIR/loom output"
                ${fakeLoomWrix.buildCommand}
                [[ "$("$out/bin/loom" --version)" == "loom 0.0.0" ]]
              )
            '');
        loom-wrix-does-not-default-launcher-override =
          scriptCheck "loom-wrix-does-not-default-launcher-override" [ ] [ ]
            (_: ''
              unset LOOM_WRIX_BIN LOOM_WRIX_SPAWN_BIN
              [[ -z "$(${fakeLoomWrix}/bin/loom --profile-environment)" ]]
              expected=$(printf 'git=%s\nraw=\n' ${fakeProfiledWrix}/bin/wrix)
              [[ "$(${fakeLoomWrix}/bin/loom --launch-inputs)" == "$expected" ]]
              export LOOM_WRIX_SPAWN_BIN=${fakeUnprofiledWrix}/bin/wrix
              expected=$(printf 'git=%s\nraw=%s\n' ${fakeProfiledWrix}/bin/wrix "$LOOM_WRIX_SPAWN_BIN")
              [[ "$(${fakeLoomWrix}/bin/loom --launch-inputs)" == "$expected" ]]
            '');
      };
    in
    {
      packages.verification-consumer-api = config.checks.loom-consumer-api;
      packages.verification-test-cargo-context = config.checks.test-cargo-context;
      checks = checks // {
        checks-match-host-system = checked.check {
          runtime = "native-process";
          resources = [ ];
          evaluation = evaluation ++ loom.testResources;
          build =
            _:
            assert pkgs.lib.all (candidate: candidate.system == system) (builtins.attrValues checks);
            pkgs.runCommand "checks-match-host-system" { } ''touch "$out"'';
        };
      };
    };
}

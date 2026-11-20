_: {
  perSystem =
    {
      config,
      pkgs,
      system,
      loom,
      ...
    }:
    let
      nativePackage = import ../native-package.nix;
      checked = import ../verification.nix;
      checkResource =
        origin:
        checked.resource {
          root = ../..;
          inherit origin;
        };
      inherit (pkgs.lib.fileset) toSource unions;
      modelFiles = unions [
        ../../specs/gate
        ../../specs/gate.md
        ../../specs/verify.md
        ../../specs/evidence.md
        ../../specs/simulation.md
        ../../scripts/test-quint-model.sh
        ../../scripts/test-quint.sh
        ../../scripts/quint-output.sh
        ../../tests/quint/model-harness.sh
        ./quint.nix
        ../quint/tools.nix
        ../quint/checker.rs
        ../quint/witnesses.rs
        ../cargo-home.nix
        ../../flake.lock
      ];
      modelSource = toSource {
        root = ../..;
        fileset = modelFiles;
      };
      modelResource = checked.resource { root = modelSource; };
      modelFixture = checked.resource {
        root = toSource {
          root = ../..;
          fileset = unions [
            modelFiles
            ../../tests/fixtures/git/test-gitconfig
          ];
        };
      };
      tools = import ../quint/tools.nix { inherit pkgs; };
      verificationTool = import ../verification-tool.nix { inherit pkgs; };
      harness = nativePackage (
        loom.craneLib.buildPackage {
          pname = "loom-quint-compiler";
          version = "1";
          src = loom.workspaceSource.path;
          inherit (loom) cargoArtifacts cargoVendorDir;
          cargoExtraArgs = "--locked --manifest-path ${loom.workspaceSource.path}/Cargo.toml --package loom-gate --example quint";
          preBuild = ''export HOME=$(mktemp -d); export CARGO_TARGET_DIR="$PWD/target"'';
          postBuild = "test ! -e ${loom.workspaceSource.path}/target";
          doCheck = false;
          installPhaseCommand = ''
            mkdir -p "$out/bin"
            cp target/release/examples/quint "$out/bin/loom-quint"
          '';
        }
      );
      installTest =
        name: target:
        pkgs.runCommand name { } ''
          mkdir -p "$out/bin"
          binary=$(${pkgs.lib.getBin pkgs.jq}/bin/jq --exit-status --raw-output \
            --arg target '${target}' --slurpfile cargo ${loom.testCompilation}/cargo.json '
            [$cargo[0].packages[] | select(.name == "loom-workflow") | .id] as $packages
            | [."rust-binaries"[] | select(."binary-name" == $target and (."package-id" as $id | $packages | index($id)))]
            | if length == 1 then .[0]."binary-path" else error("expected exactly one compiler-owned test binary") end
          ' ${loom.testCompilation}/binaries.json)
          [[ -f "$binary" && -x "$binary" ]]
          ln -s "$binary" "$out/bin/${name}"
        '';
      todoAcceptance = installTest "todo-acceptance-persistence" "todo_production";
      remediationAcceptance = installTest "remediation-acceptance" "loom_workflow";
      app = pkgs.writeShellApplication {
        name = "test-quint";
        runtimeInputs = [
          tools
          pkgs.coreutils
          pkgs.findutils
          pkgs.jq
          pkgs.git
          pkgs.bash
          pkgs.beads
          loom.dolt
          verificationTool
          pkgs.gnused
          pkgs.gnugrep
        ];
        text = ''
          export LOOM_TEST_SOURCE_ROOT=${loom.stagedSrc}
          export LOOM_TODO_ACCEPTANCE_BIN=${todoAcceptance}/bin/todo-acceptance-persistence
          export LOOM_REMEDIATION_ACCEPTANCE_BIN=${remediationAcceptance}/bin/remediation-acceptance
          export LOOM_TASK_ACCEPTANCE_BIN=${remediationAcceptance}/bin/remediation-acceptance
          export LOOM_QUINT_BIN=${harness}/bin/loom-quint
          export LOOM_QUINT_SOURCE=${loom.stagedSrc}
          export LOOM_QUINT_PACKAGE=${tools}
          ${builtins.readFile ../../scripts/test-quint.sh}
        '';
      };
      verify = import ../system-verification.nix { inherit pkgs system loom; };
      routineScenarios = [
        "reference-equivalence"
        "cache-model"
        "witnesses"
      ];
      pureModel =
        scenario:
        builtins.elem scenario (
          [
            "routine"
            "model"
          ]
          ++ routineScenarios
        );
      routine = verify {
        name = "quint-routine";
        fixtureResource = modelFixture;
        program = "${scenarioProgram "routine"}/bin/quint-routine";
      };
      scenarios = [
        "model"
        "reference-equivalence"
        "cache-model"
        "witnesses"
        "provisioning-and-batching"
        "todo-acceptance-persistence"
      ];
      scenarioProgram =
        scenario:
        let
          source = if pureModel scenario then modelFixture.path else loom.stagedSrc;
        in
        pkgs.writeShellApplication {
          name = "quint-${scenario}";
          runtimeInputs = [
            tools
            pkgs.coreutils
            pkgs.findutils
            pkgs.jq
            pkgs.git
            pkgs.bash
            pkgs.gnused
            pkgs.gnugrep
            verificationTool
          ];
          text = ''
            export LOOM_QUINT_SOURCE=${source}
            export LOOM_QUINT_PACKAGE=${tools}
            ${
              if scenario == "todo-acceptance-persistence" then
                "export LOOM_TODO_ACCEPTANCE_BIN=${todoAcceptance}/bin/todo-acceptance-persistence"
              else if scenario == "provisioning-and-batching" then
                "export LOOM_QUINT_BIN=${harness}/bin/loom-quint"
              else
                ""
            }
            exec bash ${source}/scripts/test-quint.sh '${scenario}'
          '';
        };
    in
    {
      packages = {
        quint-tools = tools;
        quint-acceptance = nativePackage harness;
        test-quint = nativePackage app;
        quint-model = checked.check {
          runtime = "native-process";
          resources = [ modelResource ];
          evaluation = import ../native-context.nix;
          build =
            paths:
            pkgs.runCommand "quint-model"
              {
                nativeBuildInputs = map pkgs.lib.getBin [
                  tools
                  pkgs.bash
                  pkgs.coreutils
                  pkgs.gnugrep
                  pkgs.jq
                  verificationTool
                ];
              }
              ''
                set -euo pipefail
                export LOOM_QUINT_SOURCE=${builtins.head paths}
                export LOOM_QUINT_PACKAGE=${tools}
                bash "$LOOM_QUINT_SOURCE/tests/quint/model-harness.sh"
                touch "$out"
              '';
        };
      }
      // builtins.listToAttrs (
        map (scenario: {
          name = "verification-quint-${scenario}";
          value =
            if builtins.elem scenario routineScenarios then
              routine
            else
              verify {
                name = "quint-${scenario}";
                fixtureResource = if pureModel scenario then modelFixture else null;
                program = "${scenarioProgram scenario}/bin/quint-${scenario}";
              };
        }) scenarios
      );
      checks.quint-model = config.packages.quint-model;
      checks.quint-source-closure = checked.check {
        runtime = "native-process";
        resources = [ modelFixture ];
        evaluation = import ../native-context.nix;
        build =
          paths:
          let
            descriptor = package: {
              root = package.drvPath;
              reporter = package.loom.reporter.drvPath;
              inherit (package.loom)
                version
                runtime
                purpose
                sources
                evaluation
                report
                ;
            };
            common = descriptor routine;
            aliases = map (scenario: config.packages."verification-quint-${scenario}") [
              "reference-equivalence"
              "cache-model"
              "witnesses"
            ];
          in
          assert pkgs.lib.all (package: descriptor package == common) aliases;
          assert common.runtime == "native-process" && common.purpose == "check";
          assert common.report.targets == [ "quint-routine" ];
          assert builtins.elem { inherit (modelFixture) origin path; } common.sources;
          assert builtins.elem {
            inherit (modelFixture) origin path;
          } config.packages.verification-quint-model.loom.sources;
          assert common.root != config.packages.verification-quint-model.drvPath;
          assert common.reporter != config.packages.verification-quint-model.loom.reporter.drvPath;
          pkgs.runCommand "quint-source-closure" { } ''
            set -euo pipefail
            [[ -f ${builtins.head paths}/specs/gate/model.qnt ]]
            [[ -f ${builtins.head paths}/scripts/quint-output.sh ]]
            [[ -x ${verificationTool}/bin/loom-verification ]]
            [[ -f ${builtins.head paths}/nix/quint/witnesses.rs ]]
            [[ -f ${builtins.head paths}/tests/fixtures/git/test-gitconfig ]]
            [[ ! -e ${builtins.head paths}/crates ]]
            [[ ! -e ${builtins.head paths}/tests/agent ]]
            touch "$out"
          '';
      };
      checks.todo-acceptance-persistence =
        let
          nativeSources = map checkResource [ "nix/private-fixture-tests.sh" ];
        in
        checked.check {
          runtime = "native-process";
          resources = nativeSources;
          evaluation = (import ../native-context.nix) ++ loom.compilerResources;
          build =
            _:
            pkgs.runCommand "todo-acceptance-persistence"
              {
                nativeBuildInputs = map pkgs.lib.getBin [ pkgs.git ];
              }
              ''
                set -euo pipefail
                LOOM_TEST_SOURCE_ROOT=${loom.stagedSrc} bash ${(builtins.elemAt nativeSources 0).path} ${todoAcceptance}/bin/todo-acceptance-persistence acceptance::
                touch "$out"
              '';
        };

      checks.remediation-acceptance =
        let
          nativeSources = map checkResource [ "nix/private-fixture-tests.sh" ];
        in
        checked.check {
          runtime = "native-process";
          resources = nativeSources;
          evaluation = (import ../native-context.nix) ++ loom.compilerResources;
          build =
            _:
            pkgs.runCommand "remediation-acceptance"
              {
                nativeBuildInputs = map pkgs.lib.getBin [ pkgs.git ];
              }
              ''
                set -euo pipefail
                LOOM_TEST_SOURCE_ROOT=${loom.stagedSrc} bash ${(builtins.elemAt nativeSources 0).path} ${remediationAcceptance}/bin/remediation-acceptance mint::tests::acceptance::
                touch "$out"
              '';
        };

      checks.phase-context =
        let
          nativeSources = map checkResource [ "nix/private-fixture-tests.sh" ];
        in
        checked.check {
          runtime = "native-process";
          resources = nativeSources;
          # The compiler preparations evaluate the staged, full test fixture.
          evaluation = (import ../native-context.nix) ++ loom.compilerResources;
          build =
            _:
            pkgs.runCommand "phase-context"
              {
                nativeBuildInputs = map pkgs.lib.getBin [
                  pkgs.bash
                  pkgs.coreutils
                  pkgs.git
                  pkgs.beads
                  loom.dolt
                  pkgs.gnused
                  pkgs.jq
                ];
              }
              ''
                set -euo pipefail
                LOOM_TEST_SOURCE_ROOT=${loom.stagedSrc} bash ${(builtins.head nativeSources).path} ${remediationAcceptance}/bin/remediation-acceptance "phase_context_tests::"
                LOOM_TEST_SOURCE_ROOT=${loom.stagedSrc} bash ${(builtins.head nativeSources).path} ${remediationAcceptance}/bin/remediation-acceptance "todo::dossier::tests::"
                LOOM_TEST_SOURCE_ROOT=${loom.stagedSrc} bash ${(builtins.head nativeSources).path} ${remediationAcceptance}/bin/remediation-acceptance "mint::acceptance::dispatch_tests::"
                LOOM_TEST_SOURCE_ROOT=${loom.stagedSrc} bash ${(builtins.head nativeSources).path} ${remediationAcceptance}/bin/remediation-acceptance "r#loop::worker::"
                touch "$out"
              '';
        };

      checks.task-acceptance-dispatch = config.checks.phase-context;

      checks.private-fixture-tests =
        let
          nativeSources = map checkResource [
            "nix/private-fixture-tests.sh"
            "tests/fixtures/private-fixture-tests.sh"
            "nix/test-scheduling.sh"
            "nix/retain-fixture-inputs.sh"
          ];
        in
        checked.check {
          runtime = "native-process";
          resources = nativeSources;
          evaluation = map checkResource [
            "nix"
            "flake.nix"
            "flake.lock"
            "rust-toolchain.toml"
            "Cargo.toml"
            "Cargo.lock"
          ];
          build =
            _:
            pkgs.runCommand "private-fixture-tests"
              {
                nativeBuildInputs = map pkgs.lib.getBin [
                  pkgs.bash
                  pkgs.coreutils
                ];
              }
              ''
                set -euo pipefail
                export LOOM_FIXTURE_INPUT_LOADER=${(builtins.elemAt nativeSources 3).path}
                bash ${(builtins.elemAt nativeSources 0).path} bash ${(builtins.elemAt nativeSources 1).path} \
                  ${(builtins.elemAt nativeSources 0).path} ${(builtins.elemAt nativeSources 2).path}
                touch "$out"
              '';
        };

      checks.quint-harness =
        let
          nativeSources = map checkResource [ "tests/quint/harness.sh" ];
        in
        checked.check {
          runtime = "native-process";
          resources = nativeSources;
          evaluation = (import ../native-context.nix) ++ loom.compilerResources;
          build =
            _:
            pkgs.runCommand "quint-harness"
              {
                nativeBuildInputs = map pkgs.lib.getBin [
                  tools
                  pkgs.bash
                  pkgs.coreutils
                  pkgs.gnugrep
                  pkgs.jq
                  verificationTool
                ];
              }
              ''
                set -euo pipefail
                export LOOM_QUINT_BIN=${harness}/bin/loom-quint
                export LOOM_QUINT_SOURCE=${loom.stagedSrc}
                export LOOM_QUINT_PACKAGE=${tools}
                    bash ${(builtins.elemAt nativeSources 0).path}
                touch "$out"
              '';
        };

      checks.quint-no-jvm =
        let
          nativeSources = map checkResource [ ];
        in
        checked.check {
          runtime = "native-process";
          resources = nativeSources;
          evaluation = (import ../native-context.nix) ++ loom.compilerResources;
          build =
            _:
            pkgs.runCommand "quint-no-jvm" { nativeBuildInputs = map pkgs.lib.getBin [ pkgs.gnugrep ]; } ''
              if grep -Ei '/[^/]*-(openjdk|jdk|jre|apalache)(-|[0-9])' ${
                pkgs.closureInfo {
                  rootPaths = [
                    tools
                    harness
                  ];
                }
              }/store-paths; then
                echo "Quint checking tools retain a forbidden JVM dependency" >&2
                exit 1
              fi
              touch "$out"
            '';
        };

      apps.test-quint = (import ../native-app.nix) pkgs {
        package = app;
        name = "test-quint";
        meta.description = "Run finite Quint acceptance scenarios.";
      };
    };
}

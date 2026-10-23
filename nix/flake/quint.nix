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
      modelSource = toSource {
        root = ../..;
        fileset = unions [
          ../../specs/gate
          ../../specs/gate.md
          ../../specs/verify.md
          ../../specs/evidence.md
          ../../specs/simulation.md
          ../../scripts/test-quint-model.sh
          ../../scripts/test-quint.sh
          ../../scripts/quint-output.sh
          ../../tests/quint/model-harness.sh
          ../../tests/quint/mutations.py
          ./quint.nix
          ../quint/tools.nix
          ../quint/checker.rs
          ../quint/witnesses.rs
          ../cargo-home.nix
          ../../flake.lock
        ];
      };
      modelResource = checked.resource { root = modelSource; };
      tools = import ../quint/tools.nix { inherit pkgs; };
      harness = loom.craneLib.buildPackage {
        pname = "loom-quint-acceptance";
        version = "1";
        src = loom.stagedSrc;
        inherit (loom) cargoArtifacts cargoVendorDir;
        cargoExtraArgs = "--locked --package loom-gate --example quint";
        doCheck = false;
        installPhaseCommand = ''
          mkdir -p "$out/bin"
          cp target/release/examples/quint "$out/bin/loom-quint"
        '';
      };
      todoAcceptance = loom.craneLib.buildPackage {
        pname = "loom-todo-acceptance";
        version = "1";
        src = loom.stagedSrc;
        inherit (loom) cargoArtifacts cargoVendorDir;
        cargoExtraArgs = "--locked --package loom-workflow --test todo_production";
        doCheck = false;
        installPhaseCommand = ''
          (
            set -euo pipefail
            mkdir -p "$out/bin"
            for binary in target/release/deps/todo_production-*; do
              if [[ -f "$binary" && -x "$binary" ]]; then
                cp "$binary" "$out/bin/todo-acceptance-persistence"
              fi
            done
            test -x "$out/bin/todo-acceptance-persistence"
          )
        '';
      };
      remediationAcceptance = loom.craneLib.buildPackage {
        pname = "loom-remediation-acceptance";
        version = "1";
        src = loom.stagedSrc;
        inherit (loom) cargoArtifacts cargoVendorDir;
        buildPhaseCargoCommand = "cargo test --release --locked --package loom-workflow --lib --no-run";
        doNotPostBuildInstallCargoBinaries = true;
        doCheck = false;
        installPhaseCommand = ''
          (
            set -euo pipefail
            mkdir -p "$out/bin"
            for binary in target/release/deps/loom_workflow-*; do
              if [[ -f "$binary" && -x "$binary" ]]; then
                cp "$binary" "$out/bin/remediation-acceptance"
              fi
            done
            [[ -x "$out/bin/remediation-acceptance" ]]
          )
        '';
      };
      app = pkgs.writeShellApplication {
        name = "test-quint";
        runtimeInputs = [
          tools
          pkgs.coreutils
          pkgs.jq
          pkgs.git
          pkgs.bash
          pkgs.beads
          pkgs.dolt
          pkgs.gnused
          pkgs.gnugrep
        ];
        text = ''
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
      hermeticScenarios = [
        "model"
        "reference-equivalence"
        "cache-model"
        "witnesses"
        "provisioning-and-batching"
        "todo-acceptance-persistence"
      ];
      scenarioProgram =
        scenario:
        pkgs.writeShellApplication {
          name = "quint-${scenario}";
          runtimeInputs = [
            tools
            pkgs.coreutils
            pkgs.jq
            pkgs.git
            pkgs.bash
            pkgs.gnused
            pkgs.gnugrep
            pkgs.python3
          ];
          text = ''
            export LOOM_QUINT_SOURCE=${loom.stagedSrc}
            export LOOM_QUINT_PACKAGE=${tools}
            ${
              if scenario == "todo-acceptance-persistence" then
                "export LOOM_TODO_ACCEPTANCE_BIN=${todoAcceptance}/bin/todo-acceptance-persistence"
              else if scenario == "provisioning-and-batching" then
                "export LOOM_QUINT_BIN=${harness}/bin/loom-quint"
              else
                ""
            }
            exec bash ${loom.stagedSrc}/scripts/test-quint.sh '${scenario}'
          '';
        };
    in
    {
      packages = {
        quint-tools = tools;
        quint-acceptance = nativePackage harness;
        test-quint = nativePackage app;
        quint-model = checked.check {
          resources = [ modelResource ];
          evaluation = import ../native-context.nix;
          build =
            paths:
            pkgs.runCommand "quint-model"
              {
                nativeBuildInputs = [
                  tools
                  pkgs.bash
                  pkgs.coreutils
                  pkgs.gnugrep
                  pkgs.jq
                  pkgs.python3
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
          value = verify {
            name = "quint-${scenario}";
            program = "${scenarioProgram scenario}/bin/quint-${scenario}";
          };
        }) hermeticScenarios
      );
      checks.quint-model = config.packages.quint-model;
      checks.quint-source-closure = checked.check {
        resources = [ modelResource ];
        evaluation = import ../native-context.nix;
        build =
          paths:
          pkgs.runCommand "quint-source-closure" { } ''
            set -euo pipefail
            [[ -f ${builtins.head paths}/specs/gate/model.qnt ]]
            [[ -f ${builtins.head paths}/scripts/quint-output.sh ]]
            [[ -f ${builtins.head paths}/tests/quint/mutations.py ]]
            [[ -f ${builtins.head paths}/nix/quint/witnesses.rs ]]
            [[ ! -e ${builtins.head paths}/crates ]]
            [[ ! -e ${builtins.head paths}/tests/agent ]]
            touch "$out"
          '';
      };
      checks.todo-acceptance-persistence =
        let
          nativeSources = map checkResource [ "scripts/run-private-fixture-tests" ];
        in
        checked.check {
          resources = nativeSources;
          evaluation = (import ../native-context.nix) ++ loom.testResources;
          build =
            _:
            pkgs.runCommand "todo-acceptance-persistence"
              {
                nativeBuildInputs = [ pkgs.git ];
              }
              ''
                set -euo pipefail
                bash ${(builtins.elemAt nativeSources 0).path} ${todoAcceptance}/bin/todo-acceptance-persistence acceptance::
                touch "$out"
              '';
        };

      checks.remediation-acceptance =
        let
          nativeSources = map checkResource [ "scripts/run-private-fixture-tests" ];
        in
        checked.check {
          resources = nativeSources;
          evaluation = (import ../native-context.nix) ++ loom.testResources;
          build =
            _:
            pkgs.runCommand "remediation-acceptance"
              {
                nativeBuildInputs = [ pkgs.git ];
              }
              ''
                set -euo pipefail
                bash ${(builtins.elemAt nativeSources 0).path} ${remediationAcceptance}/bin/remediation-acceptance mint::tests::acceptance::
                touch "$out"
              '';
        };

      checks.phase-context =
        let
          nativeSources = map checkResource [ "scripts/run-private-fixture-tests" ];
        in
        checked.check {
          resources = nativeSources;
          # The compiler preparations evaluate the staged, full test fixture.
          evaluation = (import ../native-context.nix) ++ loom.testResources;
          build =
            _:
            pkgs.runCommand "phase-context"
              {
                nativeBuildInputs = [
                  pkgs.bash
                  pkgs.coreutils
                  pkgs.git
                  pkgs.beads
                  pkgs.dolt
                  pkgs.gnused
                  pkgs.jq
                ];
              }
              ''
                set -euo pipefail
                bash ${(builtins.head nativeSources).path} ${remediationAcceptance}/bin/remediation-acceptance "phase_context_tests::"
                bash ${(builtins.head nativeSources).path} ${remediationAcceptance}/bin/remediation-acceptance "todo::dossier::tests::"
                bash ${(builtins.head nativeSources).path} ${remediationAcceptance}/bin/remediation-acceptance "mint::acceptance::dispatch_tests::"
                bash ${(builtins.head nativeSources).path} ${remediationAcceptance}/bin/remediation-acceptance "r#loop::worker::"
                touch "$out"
              '';
        };

      checks.task-acceptance-dispatch = config.checks.phase-context;

      checks.private-fixture-tests =
        let
          nativeSources = map checkResource [
            "scripts/run-private-fixture-tests"
            "tests/fixtures/private-fixture-tests.sh"
            "nix/private-fixture-tests.sh"
          ];
        in
        checked.check {
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
                nativeBuildInputs = [
                  pkgs.bash
                  pkgs.coreutils
                ];
              }
              ''
                set -euo pipefail
                for runner in ${(builtins.elemAt nativeSources 0).path} ${(builtins.elemAt nativeSources 2).path}; do
                  bash ${(builtins.elemAt nativeSources 0).path} bash ${(builtins.elemAt nativeSources 1).path} "$runner"
                done
                touch "$out"
              '';
        };

      checks.quint-harness =
        let
          nativeSources = map checkResource [ "tests/quint/harness.sh" ];
        in
        checked.check {
          resources = nativeSources;
          evaluation = (import ../native-context.nix) ++ loom.testResources;
          build =
            _:
            pkgs.runCommand "quint-harness"
              {
                nativeBuildInputs = [
                  tools
                  pkgs.bash
                  pkgs.coreutils
                  pkgs.gnugrep
                  pkgs.jq
                  pkgs.python3
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
          resources = nativeSources;
          evaluation = (import ../native-context.nix) ++ loom.testResources;
          build =
            _:
            pkgs.runCommand "quint-no-jvm" { nativeBuildInputs = [ pkgs.gnugrep ]; } ''
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

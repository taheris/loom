_: {
  perSystem =
    {
      pkgs,
      loom,
      profileManifest,
      ...
    }:
    let
      tools =
        assert pkgs.quint.version == "0.33.0";
        pkgs.quint;
      ownerArchive = ../../tests/quint/inputs/model.tar.gz;
      ownerIdentity = "92c2bd871227acc7df0a0237e47357c30acebac1fadc408b8bb25f1151907d60";
      ownerSource =
        assert builtins.hashFile "sha256" ownerArchive == ownerIdentity;
        pkgs.runCommand "quint-owner-inputs"
          {
            nativeBuildInputs = [
              pkgs.gnutar
              pkgs.gzip
            ];
          }
          ''
            mkdir -p "$out"
            tar -xzf ${ownerArchive} -C "$out"
          '';
      rpcTools =
        assert
          builtins.hashFile "sha256" ../../tests/quint/inputs/rpc/tools.nix
          == "95d0cce2e5f958ea4130831d2ca09b9bfa913acb000002b836392ca9b15f47e3";
        assert
          builtins.hashFile "sha256" ../../tests/quint/inputs/rpc/LoomApalache.java
          == "a67c5a6bdee032d564132dd0e1430fd2f90fd79b583238dbadf63bb02261c8f6";
        import ../../tests/quint/inputs/rpc/tools.nix { inherit pkgs; };
      ownerAdmission = pkgs.writeShellApplication {
        name = "quint-owner-admission";
        runtimeInputs = [
          rpcTools
          pkgs.bash
          pkgs.coreutils
          pkgs.gnugrep
          pkgs.jq
          pkgs.python3
        ];
        text = ''
          export LOOM_QUINT_OWNER_SOURCE=${ownerSource}
          export LOOM_QUINT_OWNER_IDENTITY=${ownerIdentity}
          export LOOM_QUINT_RPC_IDENTITY=fef01065313be0d7cb2b740d61844defae9b81d4
          export LOOM_APALACHE_BIN=${rpcTools}/share/quint/apalache-dist-0.62.1/apalache/bin/apalache-mc
          ${builtins.readFile ../../scripts/quint-owner-admission.sh}
        '';
      };
      harness = loom.craneLib.buildPackage {
        pname = "loom-quint-acceptance";
        version = "1";
        src = loom.stagedSrc;
        inherit (loom) cargoArtifacts;
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
        inherit (loom) cargoArtifacts;
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
        inherit (loom) cargoArtifacts;
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
      workspaceHarness = loom.craneLib.buildPackage {
        pname = "loom-workspace-acceptance";
        version = "1";
        src = loom.stagedSrc;
        inherit (loom) cargoArtifacts;
        cargoExtraArgs = "--locked --package loom --example workspace-safety-history";
        doCheck = false;
        installPhaseCommand = ''
          mkdir -p "$out/bin"
          cp target/release/examples/workspace-safety-history "$out/bin/loom-workspace-safety"
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
          pkgs.gnused
          pkgs.gnugrep
        ]
        ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.podman ];
        text = ''
          export LOOM_TODO_ACCEPTANCE_BIN=${todoAcceptance}/bin/todo-acceptance-persistence
          export LOOM_REMEDIATION_ACCEPTANCE_BIN=${remediationAcceptance}/bin/remediation-acceptance
          export LOOM_TASK_ACCEPTANCE_BIN=${remediationAcceptance}/bin/remediation-acceptance
          export LOOM_WORKSPACE_SAFETY_BIN=${workspaceHarness}/bin/loom-workspace-safety
          export LOOM_PROFILES_MANIFEST=${profileManifest}
          export LOOM_QUINT_BIN=${harness}/bin/loom-quint
          export LOOM_QUINT_SOURCE=${loom.stagedSrc}
          export LOOM_QUINT_PACKAGE=${tools}
          export LOOM_QUINT_OWNER_SOURCE=${ownerSource}
          export LOOM_QUINT_OWNER_SOLVER=${rpcTools}/share/quint/apalache-dist-0.62.1/apalache/bin/apalache-mc
          ${builtins.readFile ../../scripts/test-quint.sh}
        '';
      };
    in
    {
      packages = {
        quint-tools = tools;
        quint-owner-inputs = ownerSource;
        quint-owner-admission = ownerAdmission;
        quint-acceptance = harness;
        test-quint = app;
      };
      checks.todo-acceptance-persistence =
        pkgs.runCommand "todo-acceptance-persistence"
          {
            nativeBuildInputs = [ pkgs.git ];
          }
          ''
            set -euo pipefail
            bash ${../../scripts/run-private-fixture-tests} ${todoAcceptance}/bin/todo-acceptance-persistence acceptance::
            touch "$out"
          '';
      checks.remediation-acceptance =
        pkgs.runCommand "remediation-acceptance"
          {
            nativeBuildInputs = [ pkgs.git ];
          }
          ''
            set -euo pipefail
            bash ${../../scripts/run-private-fixture-tests} ${remediationAcceptance}/bin/remediation-acceptance mint::tests::acceptance::
            touch "$out"
          '';
      checks.phase-context =
        pkgs.runCommand "phase-context"
          {
            nativeBuildInputs = [
              pkgs.bash
              pkgs.coreutils
              pkgs.git
              pkgs.beads
              pkgs.gnused
              pkgs.jq
            ];
          }
          ''
            set -euo pipefail
            bash ${../../scripts/run-private-fixture-tests} ${remediationAcceptance}/bin/remediation-acceptance "phase_context_tests::"
            bash ${../../scripts/run-private-fixture-tests} ${remediationAcceptance}/bin/remediation-acceptance "todo::dossier::tests::"
            bash ${../../scripts/run-private-fixture-tests} ${remediationAcceptance}/bin/remediation-acceptance "mint::acceptance::dispatch_tests::"
            touch "$out"
          '';
      checks.task-acceptance-dispatch =
        pkgs.runCommand "task-acceptance-dispatch"
          {
            nativeBuildInputs = [
              pkgs.bash
              pkgs.coreutils
              pkgs.git
              pkgs.beads
              pkgs.gnused
              pkgs.jq
            ];
          }
          ''
            set -euo pipefail
            bash ${../../scripts/run-private-fixture-tests} ${remediationAcceptance}/bin/remediation-acceptance "r#loop::worker::"
            bash ${../../scripts/run-private-fixture-tests} ${remediationAcceptance}/bin/remediation-acceptance "mint::acceptance::dispatch_tests::"
            touch "$out"
          '';
      checks.private-fixture-tests =
        pkgs.runCommand "private-fixture-tests"
          {
            nativeBuildInputs = [
              pkgs.bash
              pkgs.coreutils
            ];
          }
          ''
            set -euo pipefail
            bash ${../../scripts/run-private-fixture-tests} bash ${../../tests/fixtures/private-fixture-tests.sh} ${../../scripts/run-private-fixture-tests}
            touch "$out"
          '';
      checks.quint-harness =
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
            export LOOM_QUINT_OWNER_SOURCE=${ownerSource}
            export LOOM_QUINT_OWNER_SOLVER=${rpcTools}/share/quint/apalache-dist-0.62.1/apalache/bin/apalache-mc
            bash ${../../tests/quint/harness.sh}
            touch "$out"
          '';
      checks.quint-model =
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
            export LOOM_QUINT_SOURCE=${loom.stagedSrc}
            export LOOM_QUINT_PACKAGE=${tools}
            bash ${../../tests/quint/model-harness.sh}
            touch "$out"
          '';
      checks.quint-owner-admission =
        pkgs.runCommand "quint-owner-admission"
          {
            nativeBuildInputs = [
              pkgs.bash
              pkgs.jq
            ];
          }
          ''
            export LOOM_QUINT_OWNER_ADMISSION=${ownerAdmission}/bin/quint-owner-admission
            bash ${../../tests/quint/owner-admission.sh}
            touch "$out"
          '';
      apps.quint-owner-admission = {
        type = "app";
        program = "${ownerAdmission}/bin/quint-owner-admission";
        meta.description = "Check immutable model input admission.";
      };
      apps.test-quint = {
        type = "app";
        program = "${app}/bin/test-quint";
        meta.description = "Run finite Quint acceptance scenarios.";
      };
    };
}

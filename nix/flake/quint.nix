_: {
  perSystem =
    { pkgs, loom, ... }:
    let
      tools =
        assert pkgs.quint.version == "0.32.0";
        pkgs.quint;
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
      app = pkgs.writeShellApplication {
        name = "test-quint";
        runtimeInputs = [
          tools
          pkgs.coreutils
          pkgs.jq
          pkgs.git
        ];
        text = ''
          export LOOM_TODO_ACCEPTANCE_BIN=${todoAcceptance}/bin/todo-acceptance-persistence
          export LOOM_QUINT_BIN=${harness}/bin/loom-quint
          export LOOM_QUINT_SOURCE=${loom.stagedSrc}
          export LOOM_QUINT_PACKAGE=${tools}
          ${builtins.readFile ../../scripts/test-quint.sh}
        '';
      };
    in
    {
      packages = {
        quint-tools = tools;
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
            TMPDIR=/tmp ${todoAcceptance}/bin/todo-acceptance-persistence acceptance::
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
            bash ${../../tests/quint/harness.sh}
            touch "$out"
          '';
      apps.test-quint = {
        type = "app";
        program = "${app}/bin/test-quint";
        meta.description = "Run finite Quint acceptance scenarios.";
      };
    };
}

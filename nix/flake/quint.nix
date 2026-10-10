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
      app = pkgs.writeShellApplication {
        name = "test-quint";
        runtimeInputs = [
          tools
          pkgs.coreutils
          pkgs.jq
        ];
        text = ''
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

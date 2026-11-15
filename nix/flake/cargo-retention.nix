_: {
  perSystem =
    {
      pkgs,
      loom,
      system,
      ...
    }:
    let
      checked = import ../verification.nix;
      archive = checked.artifact {
        runtime = "native-process";
        resources = [ loom.compilerSource ];
        evaluation = import ../native-context.nix;
        build =
          _:
          loom.craneLib.mkCargoDerivation (
            {
              pname = "loom-cargo-retention-archive";
              version = "1";
              src = loom.compilerSource.path;
              inherit (loom) cargoVendorDir;
              cargoArtifacts = loom.testCompilation;
              CARGO_PROFILE = "dev";
              LOOM_PREK_BIN = "${loom.hookRunner}/bin/prek";
              LOOM_ISSUES_BD_BIN = "${pkgs.beads}/bin/bd";
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
                cargo nextest archive --offline --locked --cargo-profile dev \
                  --manifest-path ${loom.compilerSource.path}/Cargo.toml \
                  -p loom-driver -p loom-workflow --lib --archive-file "$TMPDIR/retention.tar.zst"
                test ! -e ${loom.compilerSource.path}/target
              '';
              installPhaseCommand = ''
                mkdir -p "$out"
                bash ${../normalize-nextest-archive.sh} "$TMPDIR/retention.tar.zst" "$out/archive.tar.zst"
              '';
            }
            // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              LOOM_NATIVE_FLOCK_BIN = "${pkgs.util-linux}/bin/flock";
              LOOM_NATIVE_SHELL_BIN = "${pkgs.bash}/bin/bash";
              LOOM_NATIVE_STAT_BIN = "${pkgs.coreutils}/bin/stat";
            }
          );
      };
      producer = pkgs.writeShellApplication {
        name = "cargo-retention-producer";
        runtimeInputs = [
          pkgs.cargo-nextest
          pkgs.coreutils
          pkgs.bash
          pkgs.stdenv.cc
          pkgs.git
          pkgs.dolt
          loom.toolchain
        ];
        text = ''
          out="''${out:?native report output required}"
          export HOME TMPDIR
          TMPDIR="$out/temporary"
          mkdir -m 0700 "$TMPDIR"
          HOME=$(mktemp -d "$TMPDIR/home.XXXXXXXX")
          export CARGO_HOME="$HOME/cargo" CARGO_BUILD_JOBS=1
          export GIT_CONFIG_GLOBAL=${loom.stagedSrc}/tests/fixtures/git/test-gitconfig
          export GIT_CONFIG_SYSTEM=/dev/null
          export LOOM_TEST_SOURCE_ROOT=${loom.stagedSrc};
          mkdir -p "$CARGO_HOME"
          run_cases() {
            local extract
            extract=$(mktemp -d "$HOME/archive.XXXXXXXX")
            cargo-nextest nextest run --archive-file ${archive}/archive.tar.zst \
              --extract-to "$extract" --workspace-remap ${loom.stagedSrc} \
              --ignore-default-filter --test-threads 2 --no-fail-fast "$@"
          }
          status=0
          target=cargo-retention
          if [[ $# == 0 ]]; then
            run_cases -E 'test(cargo_retention) | test(worker_cargo_target_reuse_honors_policy_before_spawn)' >&2 || status=$?
          elif [[ $# == 1 && "$1" == --kernel ]]; then
            target=cargo-retention-kernel
            for case in \
              workspace::cargo::lifetime::tests::cases::native_cargo_lifetime_seals_empty_generation_and_cancels_stale_entry \
              workspace::cargo::retention::process::cases::native_cargo_retention_restart_reconciles_owned_process_generations; do
              run_cases --run-ignored only -E "test(=$case)" >&2 || status=$?
            done
          else
            exit 64
          fi
          if [[ "$status" == 0 ]]; then pass=true; else pass=false; fi
          printf '{"target":"%s","pass":%s,"evidence":"actual compiler archive; full nextest transcript retained; kernel diagnostics require explicit --kernel"}\n' "$target" "$pass"
          exit "$status"
        '';
      };
      fixtureRunner = checked.resource {
        root = ../..;
        origin = "nix/private-fixture-tests.sh";
      };
      isolated = pkgs.writeShellScript "cargo-retention-private" ''
        set -euo pipefail
        exec ${pkgs.bash}/bin/bash ${fixtureRunner.path} ${producer}/bin/cargo-retention-producer
      '';
    in
    {
      legacyPackages.kernel-cargo-retention = producer;
      packages.cargo-retention-archive = archive;
      packages.verification-cargo-retention = checked.reported {
        inherit system;
        name = "cargo-retention";
        bash = pkgs.bash;
        coreutils = pkgs.coreutils;
        resources = loom.testResources ++ [ fixtureRunner ];
        evaluation = import ../native-context.nix;
        program = isolated;
        parser = "json-lines";
        targets = [ "cargo-retention" ];
        tier = "system";
        runtime = "native-process";
      };
    };
}

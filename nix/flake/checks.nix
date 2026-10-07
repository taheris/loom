_:

{
  perSystem =
    {
      pkgs,
      imagePiCodingAgent,
      imageTreefmtWrapper,
      loom,
      sandbox,
      profileManifest,
      smokeSandbox,
      system,
      wrixLinuxPkgs,
      wrixLib,
      ...
    }:
    let
      inherit (builtins)
        all
        attrValues
        concatLists
        filter
        length
        mapAttrs
        ;
      inherit (loom)
        bin
        cargoArtifacts
        craneLib
        stagedSrc
        ;
      inherit (pkgs.lib) assertMsg makeBinPath optionalAttrs;
      loomLib = import ../lib.nix;
      testsDeriv = import ../../tests/default.nix {
        inherit pkgs smokeSandbox;
        loomPackage = loom;
      };

      checkSystemsMatchHost = all (check: check.system == system) (attrValues checks);
      checks-match-host-system =
        assert assertMsg checkSystemsMatchHost "checks.${system} contains a foreign-platform derivation";
        pkgs.runCommand "checks-match-host-system" { } ''
          set -euo pipefail
          touch "$out"
        '';

      workspace-source-includes-git-policy-scripts =
        pkgs.runCommand "workspace-source-includes-git-policy-scripts" { }
          ''
            set -euo pipefail
            for src in ${bin.src} ${stagedSrc}; do
              for script in wrix.sh sign.sh; do
                cmp "${../../crates/loom-test-support/src/git_policy}/$script" \
                  "$src/crates/loom-test-support/src/git_policy/$script"
              done
            done
            touch "$out"
          '';

      loom-gate-check = craneLib.mkCargoDerivation {
        pname = "loom-gate-check";
        version = "0.0.0";
        src = stagedSrc;
        inherit cargoArtifacts;

        doCheck = true;
        # Terminal derivation — nothing downstream consumes our `target/`,
        # so skip crane's default zstd-pack-target step on install.
        doInstallCargoArtifacts = false;

        nativeBuildInputs = [
          pkgs.git
          pkgs.cacert
          pkgs.cargo-nextest
          bin
        ];
        buildPhaseCargoCommand = "loom --version";

        preCheck = ''
          export HOME=$(mktemp -d)
          export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
        '';
        # `--tree` (every verifier, no file filter) is the explicit scope
        # for a git-less build sandbox: the source artifact has no `.git`,
        checkPhaseCargoCommand = "loom gate check --tree";
      };

      wrixProfilePackages = filter (pkg: (pkg.meta.mainProgram or "") == "wrix") sandbox.profile.packages;
      sandboxProfileEnv = wrixLinuxPkgs.buildEnv {
        name = "loom-sandbox-profile-env-check";
        paths = sandbox.profile.packages;
        pathsToLink = [ "/bin" ];
      };

      sandbox-profile-env-has-wrix =
        assert length wrixProfilePackages == 1;
        wrixLinuxPkgs.runCommand "sandbox-profile-env-has-wrix" { } ''
          set -euo pipefail
          if [[ ! -x ${sandboxProfileEnv}/bin/wrix ]]; then
            printf 'expected sandbox profile PATH to include real wrix at %s/bin/wrix\n' ${sandboxProfileEnv} >&2
            exit 1
          fi
          ${sandboxProfileEnv}/bin/wrix beads --help | grep -q 'push'
          touch "$out"
        '';

      sandbox-profile-env-has-loom = wrixLinuxPkgs.runCommand "sandbox-profile-env-has-loom" { } ''
        set -euo pipefail
        if [[ ! -x ${sandboxProfileEnv}/bin/loom ]]; then
          printf 'expected worker sandbox profile PATH to include loom at %s/bin/loom\n' ${sandboxProfileEnv} >&2
          exit 1
        fi
        ${sandboxProfileEnv}/bin/loom --version >/dev/null
        touch "$out"
      '';

      sandbox-profile-env-evaluates-nix =
        wrixLinuxPkgs.runCommand "sandbox-profile-env-evaluates-nix" { }
          ''
            set -euo pipefail
            export HOME="$TMPDIR/home"
            export NIX_CONF_DIR="$TMPDIR/nix-conf"
            export NIX_CONFIG='experimental-features ='
            mkdir -p "$HOME" "$NIX_CONF_DIR"
            [[ "$(${sandboxProfileEnv}/bin/nix --extra-experimental-features nix-command eval --offline --raw --expr '"worker-nix-ok"')" == worker-nix-ok ]]
            touch "$out"
          '';

      image-runtime-binaries-launch = wrixLinuxPkgs.runCommand "image-runtime-binaries-launch" { } ''
        set -euo pipefail
        export HOME="$TMPDIR/home"
        mkdir -p "$HOME"
        ${imagePiCodingAgent}/bin/pi --version >/dev/null
        ${imageTreefmtWrapper}/bin/treefmt --version >/dev/null
        ${wrixLinuxPkgs.podman}/bin/podman --version >/dev/null
        touch "$out"
      '';

      fakePodman = pkgs.writeShellScript "podman" ''
        set -euo pipefail
        printf 'Error: opening /dev/net/tun: no such file\n' >&2
        exit 125
      '';

      fakeSandboxImage = pkgs.writeShellScript "fake-sandbox-image" ''
        set -euo pipefail
        printf 'fake image payload\n'
        printf 'Adding base layer 1 from fake/layer.tar\n' >&2
      '';

      fakePodmanRunOciPermissionDenied = pkgs.writeShellScript "podman" ''
        set -euo pipefail
        cmd=""
        for arg in "$@"; do
          case "$arg" in
            info | load | run)
              cmd="$arg"
              break
              ;;
          esac
        done
        case "$cmd" in
          info)
            exit 0
            ;;
          load)
            cat >/dev/null
            printf 'Loaded image: localhost/fake:latest\n'
            ;;
          run)
            printf 'Error: crun: mount `proc` to `proc`: OCI permission denied\n' >&2
            exit 126
            ;;
          *)
            printf 'unexpected podman args: %s\n' "$*" >&2
            exit 2
            ;;
        esac
      '';

      fakePodmanCreatesReadOnlyStorage = pkgs.writeShellScript "podman" ''
        set -euo pipefail
        if [[ -n "''${LOOM_TEST_PODMAN_ARGS_LOG:-}" ]]; then
          for arg in "$@"; do
            printf '<%s>' "$arg" >> "$LOOM_TEST_PODMAN_ARGS_LOG"
          done
          printf '\n' >> "$LOOM_TEST_PODMAN_ARGS_LOG"
        fi
        root=""
        cmd=""
        while [[ "$#" -gt 0 ]]; do
          case "$1" in
            --root)
              root="$2"
              shift 2
              ;;
            --runroot)
              shift 2
              ;;
            info | load | run)
              cmd="$1"
              shift
              break
              ;;
            *)
              shift
              ;;
          esac
        done
        case "$cmd" in
          info)
            exit 0
            ;;
          load)
            cat >/dev/null
            printf 'Loaded image: localhost/fake:latest\n'
            ;;
          run)
            if [[ -z "$root" ]]; then
              printf 'expected --root argument\n' >&2
              exit 2
            fi
            readonly_dir="$root/overlay/fake/diff/nix/store/fake-lib/lib"
            mkdir -p "$readonly_dir"
            touch "$readonly_dir/libfake.so"
            chmod -R a-w "$root/overlay/fake/diff/nix/store/fake-lib"
            printf 'sandbox-checks-started\n'
            case "''${LOOM_TEST_SANDBOX_RESULT:-complete}" in
              check-failure)
                printf 'hook check failed: operation not permitted\n' >&2
                exit 1
                ;;
              health-only)
                printf 'sandbox-agent-health-ok\n'
                ;;
              nix-eval-only)
                printf '%s\n' sandbox-agent-health-ok sandbox-nix-eval-ok
                ;;
              hooks-only)
                printf '%s\n' sandbox-agent-health-ok sandbox-commit-hooks-ok sandbox-pre-push-hooks-ok sandbox-hook-self-tests-ok
                ;;
              nix-only)
                printf '%s\n' sandbox-agent-health-ok sandbox-nix-eval-ok sandbox-nix-build-ok
                ;;
              complete)
                printf '%s\n' sandbox-agent-health-ok sandbox-nix-eval-ok sandbox-nix-build-ok sandbox-commit-hooks-ok sandbox-pre-push-hooks-ok sandbox-hook-self-tests-ok
                ;;
              *)
                printf 'unknown fake sandbox result\n' >&2
                exit 2
                ;;
            esac
            ;;
          *)
            printf 'unexpected podman args: %s\n' "$*" >&2
            exit 2
            ;;
        esac
      '';

      test-app-ignores-host-git-signing = testsDeriv.test-app-ignores-host-git-signing;

      smoke-beads-fixture =
        pkgs.runCommand "smoke-beads-fixture"
          {
            nativeBuildInputs = [
              pkgs.bash
              pkgs.beads
              pkgs.git
              pkgs.jq
            ];
          }
          ''
            set -euo pipefail
            bash ${../../tests/smoke/seed-beads-test.sh} ${../../tests/smoke/seed-beads.sh}
            touch "$out"
          '';

      fakeSmokeNix = pkgs.writeShellScriptBin "nix" ''
        set -euo pipefail
        touch "$SMOKE_NIX_CALLED"
        exit 1
      '';

      smoke-preflight-skips-runtime-build = pkgs.runCommand "smoke-preflight-skips-runtime-build" { } ''
        set -euo pipefail
        export PATH="${fakeSmokeNix}/bin:$PATH"
        export SMOKE_NIX_CALLED="$TMPDIR/nix-called"
        set +e
        script_output=$(${pkgs.bash}/bin/bash ${../../scripts/run-smoke.sh} \
          "$TMPDIR/missing-fuse" "$TMPDIR/missing-tun" .#smoke-runtime 2>&1)
        rc=$?
        set -e
        if [[ "$rc" -ne 77 ]]; then
          printf 'expected smoke preflight skip exit 77, got %s\n%s\n' "$rc" "$script_output" >&2
          exit 1
        fi
        if [[ -e "$SMOKE_NIX_CALLED" ]]; then
          printf 'smoke preflight realized the runtime before checking devices\n' >&2
          exit 1
        fi
        case "$script_output" in
          *"skip: container runtime devices"*)
            ;;
          *)
            printf 'expected smoke preflight skip output, got:\n%s\n' "$script_output" >&2
            exit 1
            ;;
        esac
        touch "$out"
      '';

      test-sandbox-skips-unsupported-runtime =
        pkgs.runCommand "test-sandbox-skips-unsupported-runtime" { }
          ''
            set -euo pipefail
            fakebin=$(mktemp -d)
            ln -s ${fakePodman} "$fakebin/podman"
            export PATH="$fakebin:${
              makeBinPath [
                pkgs.coreutils
                pkgs.gnugrep
                pkgs.gnused
                pkgs.bash
              ]
            }"
            set +e
            script_output=$(bash ${../../scripts/test-sandbox.sh} 2>&1)
            rc=$?
            set -e
            if [[ "$rc" -ne 77 ]]; then
              printf 'expected test-sandbox skip exit 77, got %s\n%s\n' "$rc" "$script_output" >&2
              exit 1
            fi
            case "$script_output" in
              *"test-sandbox: skipped"*)
                ;;
              *)
                printf 'expected test-sandbox skip output, got:\n%s\n' "$script_output" >&2
                exit 1
                ;;
            esac
            touch "$out"
          '';

      test-sandbox-skips-oci-permission-denied =
        pkgs.runCommand "test-sandbox-skips-oci-permission-denied" { }
          ''
            set -euo pipefail
            fakebin=$(mktemp -d)
            ln -s ${fakePodmanRunOciPermissionDenied} "$fakebin/podman"
            export PATH="$fakebin:${
              makeBinPath [
                pkgs.coreutils
                pkgs.gnugrep
                pkgs.gnused
                pkgs.bash
              ]
            }"
            export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
            export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
            export LOOM_TEST_SANDBOX_SOURCE=${stagedSrc}
            export WRIX_PREK_HOOKS=${wrixLib.prekHooks}
            set +e
            script_output=$(bash ${../../scripts/test-sandbox.sh} 2>&1)
            rc=$?
            set -e
            if [[ "$rc" -ne 77 ]]; then
              printf 'expected test-sandbox skip exit 77, got %s\n%s\n' "$rc" "$script_output" >&2
              exit 1
            fi
            case "$script_output" in
              *"test-sandbox: skipped"*"OCI permission denied"*)
                ;;
              *)
                printf 'expected OCI permission skip output, got:\n%s\n' "$script_output" >&2
                exit 1
                ;;
            esac
            touch "$out"
          '';

      test-sandbox-ignores-read-only-podman-storage-cleanup =
        pkgs.runCommand "test-sandbox-ignores-read-only-podman-storage-cleanup" { }
          ''
            set -euo pipefail
            fakebin=$(mktemp -d)
            ln -s ${fakePodmanCreatesReadOnlyStorage} "$fakebin/podman"
            export PATH="$fakebin:${
              makeBinPath [
                pkgs.coreutils
                pkgs.gnugrep
                pkgs.gnused
                pkgs.bash
              ]
            }"
            export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
            export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
            export LOOM_TEST_SANDBOX_SOURCE=${stagedSrc}
            export WRIX_PREK_HOOKS=${wrixLib.prekHooks}
            set +e
            script_output=$(bash ${../../scripts/test-sandbox.sh} 2>&1)
            rc=$?
            set -e
            if [[ "$rc" -ne 0 ]]; then
              printf 'expected test-sandbox success despite read-only podman storage cleanup, got %s\n%s\n' "$rc" "$script_output" >&2
              exit 1
            fi
            touch "$out"
          '';

      test-sandbox-disables-container-network =
        pkgs.runCommand "test-sandbox-disables-container-network" { }
          ''
            set -euo pipefail
            fakebin=$(mktemp -d)
            ln -s ${fakePodmanCreatesReadOnlyStorage} "$fakebin/podman"
            export PATH="$fakebin:${
              makeBinPath [
                pkgs.coreutils
                pkgs.gnugrep
                pkgs.gnused
                pkgs.bash
              ]
            }"
            export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
            export LOOM_TEST_PODMAN_ARGS_LOG="$TMPDIR/podman-args"
            export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
            export LOOM_TEST_SANDBOX_SOURCE=${stagedSrc}
            export WRIX_PREK_HOOKS=${wrixLib.prekHooks}
            bash ${../../scripts/test-sandbox.sh}
            if [[ $(<"$LOOM_TEST_PODMAN_ARGS_LOG") != *'<run><--rm><--network=none><--entrypoint></bin/bash>'* ]]; then
              printf 'expected test-sandbox podman run to disable networking and select the health-check entrypoint; observed:\n%s\n' "$(<"$LOOM_TEST_PODMAN_ARGS_LOG")" >&2
              exit 1
            fi
            touch "$out"
          '';

      test-sandbox-rejects-incomplete-checks =
        pkgs.runCommand "test-sandbox-rejects-incomplete-checks" { }
          ''
            set -euo pipefail
            fakebin=$(mktemp -d)
            ln -s ${fakePodmanCreatesReadOnlyStorage} "$fakebin/podman"
            export PATH="$fakebin:${
              makeBinPath [
                pkgs.bash
                pkgs.coreutils
                pkgs.gnugrep
                pkgs.gnused
              ]
            }"
            export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
            export LOOM_TEST_SANDBOX_SOURCE=${stagedSrc}
            export WRIX_PREK_HOOKS=${wrixLib.prekHooks}
            export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
            for result in health-only nix-eval-only hooks-only nix-only check-failure; do
              export LOOM_TEST_SANDBOX_RESULT="$result"
              status=0
              bash ${../../scripts/test-sandbox.sh} > "$TMPDIR/output" 2>&1 || status=$?
              if [[ "$status" -ne 1 ]]; then
                printf 'expected failed sandbox checks, not success or skip (%s):\n%s\n' "$status" "$(<"$TMPDIR/output")" >&2
                exit 1
              fi
              case "$result" in
                health-only | hooks-only)
                  grep -Fq 'sandbox-nix-eval-ok canary missing' "$TMPDIR/output"
                  ;;
                nix-eval-only)
                  grep -Fq 'sandbox-nix-build-ok canary missing' "$TMPDIR/output"
                  ;;
                nix-only)
                  grep -Fq 'sandbox-commit-hooks-ok canary missing' "$TMPDIR/output"
                  ;;
                check-failure)
                  grep -Fq 'sandbox verification failed' "$TMPDIR/output"
                  ;;
              esac
            done
            touch "$out"
          '';

      test-sandbox-needs-no-dolt-socket = pkgs.runCommand "test-sandbox-needs-no-dolt-socket" { } ''
        set -euo pipefail
        fakebin=$(mktemp -d)
        ln -s ${fakePodmanCreatesReadOnlyStorage} "$fakebin/podman"
        export PATH="$fakebin:${
          makeBinPath [
            pkgs.bash
            pkgs.coreutils
            pkgs.gnugrep
            pkgs.gnused
          ]
        }"
        export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
        export LOOM_TEST_SANDBOX_SOURCE=${stagedSrc}
        export WRIX_PREK_HOOKS=${wrixLib.prekHooks}
        export LOOM_TEST_PODMAN_ARGS_LOG="$TMPDIR/podman-args"
        export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
        bash ${../../scripts/test-sandbox.sh}
        observed=$(<"$LOOM_TEST_PODMAN_ARGS_LOG")
        if [[ "$observed" == *'BEADS_DOLT_SERVER_SOCKET'* || "$observed" == *'.wrix/dolt.sock'* ]]; then
          printf 'packaged-agent health check unexpectedly depends on Dolt:\n%s\n' "$observed" >&2
          exit 1
        fi
        touch "$out"
      '';

      profileManifestEntries = concatLists (
        attrValues (mapAttrs (_profile: attrValues) profileManifest.passthru.manifest)
      );
      profileManifestKeepsRuntimePathContext = all (
        entry:
        builtins.hasContext entry.source
        && builtins.hasContext entry.launcher
        && builtins.hasContext entry.profile_config
      ) profileManifestEntries;
      profile-manifest-keeps-runtime-path-context =
        assert profileManifestKeepsRuntimePathContext;
        pkgs.runCommand "profile-manifest-keeps-runtime-path-context" { } ''
          touch "$out"
        '';

      fakeLoomBin = pkgs.writeShellApplication {
        name = "loom";
        text = ''
          if [[ "''${1:-}" == --launch-inputs ]]; then
            printf 'git=%s\nraw=%s\n' "''${LOOM_WRIX_BIN:-}" "''${LOOM_WRIX_SPAWN_BIN:-}"
          else
            printf 'loom 0.0.0\n'
          fi
        '';
      };
      fakeUnprofiledWrix = pkgs.writeShellApplication {
        name = "wrix";
        text = ''
          printf 'unprofiled wrix %s\n' "$*"
        '';
      };
      fakeProfiledWrix =
        (pkgs.writeShellApplication {
          name = "wrix";
          text = ''
            exec ${fakeUnprofiledWrix}/bin/wrix --profile-config /nix/store/fake-profile.json "$@"
          '';
        }).overrideAttrs
          (old: {
            passthru = (old.passthru or { }) // {
              launcher = fakeUnprofiledWrix;
            };
          });
      fakeProfileManifest = pkgs.writeText "profile-images.json" "{}";
      fakeLoomWrix = loomLib.mkLoomBin {
        inherit pkgs;
        loomBuild = {
          bin = fakeLoomBin;
        };
        wrixLauncher = fakeProfiledWrix;
        profileManifest = fakeProfileManifest;
      };
      loom-wrapper-preserves-spaced-output-path =
        pkgs.runCommand "loom-wrapper-preserves-spaced-output-path"
          {
            nativeBuildInputs = [ pkgs.makeWrapper ];
          }
          ''
            set -euo pipefail
            (
              out="$TMPDIR/loom output"
              ${fakeLoomWrix.buildCommand}
              [[ "$("$out/bin/loom" --version)" == "loom 0.0.0" ]]
            )
            touch "$out"
          '';

      loom-wrix-does-not-default-launcher-override =
        pkgs.runCommand "loom-wrix-does-not-default-launcher-override" { }
          ''
            set -euo pipefail
            unset LOOM_WRIX_BIN LOOM_WRIX_SPAWN_BIN
            expected=$(printf 'git=%s\nraw=\n' ${fakeProfiledWrix}/bin/wrix)
            [[ "$(${fakeLoomWrix}/bin/loom --launch-inputs)" == "$expected" ]]
            export LOOM_WRIX_SPAWN_BIN=${fakeUnprofiledWrix}/bin/wrix
            expected=$(printf 'git=%s\nraw=%s\n' ${fakeProfiledWrix}/bin/wrix "$LOOM_WRIX_SPAWN_BIN")
            [[ "$(${fakeLoomWrix}/bin/loom --launch-inputs)" == "$expected" ]]
            touch "$out"
          '';

      wrix-requires-explicit-profile-config =
        pkgs.runCommand "wrix-requires-explicit-profile-config" { }
          ''
            set -euo pipefail
            launcher=${sandbox.launcher}/bin/wrix
            "$launcher" --profile-config /selected-profile.json run --help > "$TMPDIR/help"
            ${pkgs.gnugrep}/bin/grep -q -- '--profile-config' "$TMPDIR/help"
            if WRIX_DEFAULT_IMAGE_REF=ignored WRIX_DEFAULT_IMAGE_SOURCE=ignored \
              "$launcher" run "$TMPDIR" true > "$TMPDIR/no-config" 2>&1; then
              printf 'raw Wrix accepted a session without explicit profile config\n' >&2
              exit 1
            fi
            ${pkgs.gnugrep}/bin/grep -q -- '--profile-config' "$TMPDIR/no-config"
            touch "$out"
          '';

      checks = {
        inherit
          loom-gate-check
          loom-wrapper-preserves-spaced-output-path
          loom-wrix-does-not-default-launcher-override
          wrix-requires-explicit-profile-config
          profile-manifest-keeps-runtime-path-context
          smoke-beads-fixture
          smoke-preflight-skips-runtime-build
          test-app-ignores-host-git-signing
          test-sandbox-disables-container-network
          test-sandbox-ignores-read-only-podman-storage-cleanup
          test-sandbox-needs-no-dolt-socket
          test-sandbox-rejects-incomplete-checks
          test-sandbox-skips-oci-permission-denied
          test-sandbox-skips-unsupported-runtime
          workspace-source-includes-git-policy-scripts
          ;
      }
      // optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        inherit (testsDeriv) smoke-git-policy smoke-host-gate;
        inherit
          image-runtime-binaries-launch
          sandbox-profile-env-has-loom
          sandbox-profile-env-has-wrix
          sandbox-profile-env-evaluates-nix
          ;
      };
    in
    {
      checks = checks // {
        inherit checks-match-host-system;
      };
    };
}

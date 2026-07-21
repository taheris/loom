_:

{
  perSystem =
    {
      pkgs,
      loom,
      sandbox,
      profileManifest,
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
      inherit (pkgs.lib) makeBinPath;
      loomLib = import ../lib.nix;
      testsDeriv = import ../../tests/default.nix {
        inherit pkgs;
        loomPackage = loom;
      };

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
      sandboxProfileEnv = pkgs.buildEnv {
        name = "loom-sandbox-profile-env-check";
        paths = sandbox.profile.packages;
        pathsToLink = [ "/bin" ];
      };

      sandbox-profile-env-has-wrix =
        assert length wrixProfilePackages == 1;
        pkgs.runCommand "sandbox-profile-env-has-wrix" { } ''
          set -euo pipefail
          if [[ ! -x ${sandboxProfileEnv}/bin/wrix ]]; then
            printf 'expected sandbox profile PATH to include real wrix at %s/bin/wrix\n' ${sandboxProfileEnv} >&2
            exit 1
          fi
          ${sandboxProfileEnv}/bin/wrix beads --help | grep -q 'push'
          touch "$out"
        '';

      sandbox-profile-env-has-loom = pkgs.runCommand "sandbox-profile-env-has-loom" { } ''
        set -euo pipefail
        if [[ ! -x ${sandboxProfileEnv}/bin/loom ]]; then
          printf 'expected worker sandbox profile PATH to include loom at %s/bin/loom\n' ${sandboxProfileEnv} >&2
          exit 1
        fi
        ${sandboxProfileEnv}/bin/loom --version >/dev/null
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
            printf 'sandbox-hook-chain-ok\n'
            ;;
          *)
            printf 'unexpected podman args: %s\n' "$*" >&2
            exit 2
            ;;
        esac
      '';

      test-app-ignores-host-git-signing = testsDeriv.test-app-ignores-host-git-signing;

      test-sandbox-skips-unsupported-runtime =
        pkgs.runCommand "test-sandbox-skips-unsupported-runtime" { }
          ''
            set -euo pipefail
            fakebin=$(mktemp -d)
            ln -s ${fakePodman} "$fakebin/podman"
            export PATH="$fakebin:${
              makeBinPath [
                pkgs.coreutils
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
                pkgs.gnused
                pkgs.bash
              ]
            }"
            export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
            export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
            export LOOM_TEST_SANDBOX_SOURCE="$TMPDIR/source"
            export LOOM_TEST_SANDBOX_WORKSPACE="$LOOM_TEST_SANDBOX_SOURCE"
            mkdir -p "$LOOM_TEST_SANDBOX_SOURCE/.git"
            touch "$LOOM_TEST_SANDBOX_SOURCE/Cargo.toml"
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
                pkgs.gnused
                pkgs.bash
              ]
            }"
            export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
            export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
            export LOOM_TEST_SANDBOX_SOURCE="$TMPDIR/source"
            export LOOM_TEST_SANDBOX_WORKSPACE="$LOOM_TEST_SANDBOX_SOURCE"
            mkdir -p "$LOOM_TEST_SANDBOX_SOURCE/.git"
            touch "$LOOM_TEST_SANDBOX_SOURCE/Cargo.toml"
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
                pkgs.gnused
                pkgs.bash
              ]
            }"
            export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
            export LOOM_TEST_PODMAN_ARGS_LOG="$TMPDIR/podman-args"
            export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
            export LOOM_TEST_SANDBOX_SOURCE="$TMPDIR/source"
            export LOOM_TEST_SANDBOX_WORKSPACE="$LOOM_TEST_SANDBOX_SOURCE"
            mkdir -p "$LOOM_TEST_SANDBOX_SOURCE/.git"
            touch "$LOOM_TEST_SANDBOX_SOURCE/Cargo.toml"
            bash ${../../scripts/test-sandbox.sh}
            if [[ $(<"$LOOM_TEST_PODMAN_ARGS_LOG") != *'<run><--rm><--network=none><--env><WRIX_AGENT=pi>'* ]]; then
              printf 'expected test-sandbox podman run to disable networking and select Pi; observed:\n%s\n' "$(<"$LOOM_TEST_PODMAN_ARGS_LOG")" >&2
              exit 1
            fi
            touch "$out"
          '';

      test-sandbox-mounts-host-dolt-socket = pkgs.runCommand "test-sandbox-mounts-host-dolt-socket" { } ''
        set -euo pipefail
        fakebin=$(mktemp -d)
        ln -s ${fakePodmanCreatesReadOnlyStorage} "$fakebin/podman"
        export PATH="$fakebin:${
          makeBinPath [
            pkgs.bash
            pkgs.coreutils
            pkgs.gnused
            pkgs.jq
          ]
        }"
        export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
        export LOOM_TEST_PODMAN_ARGS_LOG="$TMPDIR/podman-args"
        export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
        export LOOM_TEST_SANDBOX_SOURCE="$TMPDIR/source"
        export LOOM_TEST_SANDBOX_WORKSPACE="$LOOM_TEST_SANDBOX_SOURCE"
        mkdir -p \
          "$LOOM_TEST_SANDBOX_SOURCE/.beads" \
          "$LOOM_TEST_SANDBOX_SOURCE/.git" \
          "$LOOM_TEST_SANDBOX_SOURCE/.wrix"
        touch "$LOOM_TEST_SANDBOX_SOURCE/Cargo.toml"
        printf '%s\n' '{"backend":"dolt"}' > "$LOOM_TEST_SANDBOX_SOURCE/.beads/metadata.json"
        ${pkgs.python3}/bin/python3 - "$LOOM_TEST_SANDBOX_SOURCE/.wrix/dolt.sock" <<'PY'
        import socket
        import sys

        sock = socket.socket(socket.AF_UNIX)
        sock.bind(sys.argv[1])
        sock.close()
        PY
        bash ${../../scripts/test-sandbox.sh}
        observed=$(<"$LOOM_TEST_PODMAN_ARGS_LOG")
        expected="<--volume><$LOOM_TEST_SANDBOX_SOURCE/.wrix/dolt.sock:/workspace/.wrix/dolt.sock>"
        if [[ "$observed" != *"$expected"* ]]; then
          printf 'expected test-sandbox to mount the host Dolt socket; observed:\n%s\n' "$observed" >&2
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
        && (!(entry ? launcher) || builtins.hasContext entry.launcher)
        && (!(entry ? profile_config) || builtins.hasContext entry.profile_config)
      ) profileManifestEntries;
      profile-manifest-keeps-runtime-path-context =
        assert profileManifestKeepsRuntimePathContext;
        pkgs.runCommand "profile-manifest-keeps-runtime-path-context" { } ''
          touch "$out"
        '';

      fakeLoomBin = pkgs.writeShellApplication {
        name = "loom";
        text = ''
          printf 'loom 0.0.0\n'
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
      loom-wrix-uses-unprofiled-spawn-launcher =
        pkgs.runCommand "loom-wrix-uses-unprofiled-spawn-launcher" { }
          ''
            set -euo pipefail
            grep=${pkgs.gnugrep}/bin/grep
            wrapper=${fakeLoomWrix}/bin/loom
            profiled=${fakeProfiledWrix}/bin/wrix
            unprofiled=${fakeUnprofiledWrix}/bin/wrix
            "$grep" -qF "LOOM_WRIX_BIN" "$wrapper"
            "$grep" -qF "LOOM_WRIX_SPAWN_BIN" "$wrapper"
            "$grep" -qF "$profiled" "$wrapper"
            "$grep" -qF "$unprofiled" "$wrapper"
            "$grep" -q -- '--profile-config' "$profiled"
            if "$grep" -q -- '--profile-config' "$unprofiled"; then
              printf 'LOOM_WRIX_SPAWN_BIN must point at the unprofiled wrix launcher, not %s\n' "$unprofiled" >&2
              exit 1
            fi
            touch "$out"
          '';
    in
    {
      checks = {
        inherit
          loom-gate-check
          loom-wrix-uses-unprofiled-spawn-launcher
          profile-manifest-keeps-runtime-path-context
          sandbox-profile-env-has-loom
          sandbox-profile-env-has-wrix
          test-app-ignores-host-git-signing
          test-sandbox-disables-container-network
          test-sandbox-ignores-read-only-podman-storage-cleanup
          test-sandbox-mounts-host-dolt-socket
          test-sandbox-skips-oci-permission-denied
          test-sandbox-skips-unsupported-runtime
          ;
      };
    };
}

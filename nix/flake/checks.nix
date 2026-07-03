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
            ;;
          *)
            printf 'unexpected podman args: %s\n' "$*" >&2
            exit 2
            ;;
        esac
      '';

      test-sandbox-skips-unsupported-runtime =
        pkgs.runCommand "test-sandbox-skips-unsupported-runtime" { }
          ''
            set -euo pipefail
            fakebin=$(mktemp -d)
            ln -s ${fakePodman} "$fakebin/podman"
            export PATH="$fakebin:${
              pkgs.lib.makeBinPath [
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
              pkgs.lib.makeBinPath [
                pkgs.coreutils
                pkgs.gnused
                pkgs.bash
              ]
            }"
            export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
            export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
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
              pkgs.lib.makeBinPath [
                pkgs.coreutils
                pkgs.gnused
                pkgs.bash
              ]
            }"
            export LOOM_SANDBOX_IMAGE=${fakeSandboxImage}
            export LOOM_TEST_SANDBOX_SKIP_DEVICE_CHECKS=1
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

      profileManifestEntries = concatLists (
        attrValues (mapAttrs (_profile: attrValues) profileManifest.passthru.manifest)
      );
      profileManifestKeepsRuntimePathContext = all (
        entry:
        builtins.hasContext entry.source
        && (!(entry ? profile_config) || builtins.hasContext entry.profile_config)
      ) profileManifestEntries;
      profile-manifest-keeps-runtime-path-context =
        assert profileManifestKeepsRuntimePathContext;
        pkgs.runCommand "profile-manifest-keeps-runtime-path-context" { } ''
          touch "$out"
        '';
    in
    {
      checks = {
        inherit
          loom-gate-check
          profile-manifest-keeps-runtime-path-context
          sandbox-profile-env-has-loom
          sandbox-profile-env-has-wrix
          test-sandbox-ignores-read-only-podman-storage-cleanup
          test-sandbox-skips-oci-permission-denied
          test-sandbox-skips-unsupported-runtime
          ;
      };
    };
}

_:

{
  perSystem =
    { pkgs, loom, ... }:
    let
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

      fakePodman = pkgs.writeShellScript "podman" ''
        set -euo pipefail
        printf 'Error: opening /dev/net/tun: no such file\n' >&2
        exit 125
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
    in
    {
      checks = {
        inherit loom-gate-check test-sandbox-skips-unsupported-runtime;
      };
    };
}

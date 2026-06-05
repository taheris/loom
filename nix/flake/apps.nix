# Exposes user-facing `nix run` entry points:
#
# - `.#test`: container smoke harness.
#   Linux runs `tests/run-tests.sh`; Darwin returns a no-op stub.
# - `.#test-sandbox`: boots `.#sandbox` and checks the selected Pi runtime.
#   Linux only; Darwin returns a no-op stub.
# - `.#fuzz-loom`: on-demand `cargo fuzz` driver.
#   This is intentionally not gated by `nix flake check`.
#
# Spec: specs/tests.md § CI integration / Cross-platform.
_:

{
  perSystem =
    {
      pkgs,
      loom,
      sandbox,
      ...
    }:
    let
      inherit (pkgs.stdenv.hostPlatform) isLinux;

      testsDeriv = import ../../tests/default.nix {
        inherit pkgs;
        loomPackage = loom;
      };

      smokeApp = testsDeriv.loom-smoke;

      fuzzApp = pkgs.writeShellApplication {
        name = "fuzz-loom";
        runtimeInputs = [
          pkgs.cargo-fuzz
          loom.toolchain
        ];
        text = ''
          if [ "$#" -eq 0 ]; then
            echo "usage: nix run .#fuzz-loom -- <fuzz-target> [cargo-fuzz args...]" >&2
            exit 64
          fi
          exec cargo fuzz "$@"
        '';
      };

      sandboxSmokeLinux = pkgs.writeShellApplication {
        name = "test-sandbox";
        runtimeInputs = [ pkgs.podman ];
        text = ''
          load_out=$("${sandbox.image}" | podman load)
          ref=$(printf '%s\n' "$load_out" | sed -nE 's/^Loaded image: (.+)$/\1/p' | head -n1)
          if [[ -z "$ref" ]]; then
            printf 'test-sandbox: could not parse image ref from podman load output:\n%s\n' "$load_out" >&2
            exit 1
          fi

          podman run --rm --entrypoint=/bin/bash "$ref" -c '
            set -uo pipefail
            if ! out=$(pi --version 2>&1); then
              printf "test-sandbox: pi --version failed: %s\n" "$out" >&2
              exit 1
            fi
          '
        '';
      };

      sandboxSmokeDarwin = pkgs.writeShellApplication {
        name = "test-sandbox";
        text = ''
          echo "test-sandbox not available on Darwin"
          exit 0
        '';
      };

      sandboxSmokeApp = if isLinux then sandboxSmokeLinux else sandboxSmokeDarwin;
    in
    {
      apps = {
        test = {
          type = "app";
          program = "${smokeApp}/bin/test";
          meta.description = "Container smoke harness (Linux only; Darwin stub)";
        };
        test-sandbox = {
          type = "app";
          program = "${sandboxSmokeApp}/bin/test-sandbox";
          meta.description = "Runtime check that the selected Pi agent responds to --version inside the sandbox image (Linux only; Darwin stub)";
        };
        fuzz-loom = {
          type = "app";
          program = "${fuzzApp}/bin/fuzz-loom";
          meta.description = "On-demand cargo fuzz runner (not gated by flake check)";
        };
      };
    };
}

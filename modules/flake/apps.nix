# modules/flake/apps.nix
#
# Exposes the user-facing `nix run` entry points:
#
#   nix run .#test                — container smoke harness. On Linux,
#                                   runs the real `writeShellApplication`
#                                   wrapper around tests/run-tests.sh. On
#                                   Darwin, runs a stub that exits 0 with
#                                   "container smoke not available on
#                                   Darwin" so the entry point is a no-op
#                                   rather than missing.
#   nix run .#test-sandbox  — boots the built `.#sandbox` image
#                                   once and asserts the three agent
#                                   binaries (`pi`, `claude`,
#                                   `loom-direct-runner`) each respond
#                                   to `--version`. Linux only; Darwin
#                                   returns a stub. Spec verifier for
#                                   `specs/agent.md` § Agent runtime
#                                   layer.
#   nix run .#fuzz-loom           — on-demand `cargo fuzz` driver. NOT
#                                   gated by `nix flake check` (per spec
#                                   § Property-based testing: "No
#                                   `cargo fuzz` under `nix flake
#                                   check`"); intended for nightly or
#                                   local exhaustive runs only.
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
            rc=0
            for bin in pi claude loom-direct-runner; do
              if ! out=$("$bin" --version 2>&1); then
                printf "test-sandbox: %s --version failed: %s\n" "$bin" "$out" >&2
                rc=1
              fi
            done
            exit "$rc"
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
          meta.description = "Runtime check that pi/claude/loom-direct-runner respond to --version inside the sandbox image (Linux only; Darwin stub)";
        };
        fuzz-loom = {
          type = "app";
          program = "${fuzzApp}/bin/fuzz-loom";
          meta.description = "On-demand cargo fuzz runner (not gated by flake check)";
        };
      };
    };
}

# modules/flake/apps.nix
#
# Exposes the user-facing `nix run` entry points:
#
#   nix run .#test       — container smoke harness. On Linux, runs the
#                          real `writeShellApplication` wrapper around
#                          tests/run-tests.sh. On Darwin, runs a stub
#                          that exits 0 with "container smoke not
#                          available on Darwin" so the entry point is
#                          a no-op rather than missing.
#   nix run .#fuzz-loom  — on-demand `cargo fuzz` driver. NOT gated by
#                          `nix flake check` (per spec § Property-based
#                          testing: "No `cargo fuzz` under `nix flake
#                          check`"); intended for nightly or local
#                          exhaustive runs only.
#
# Spec: specs/tests.md § CI integration / Cross-platform.
_:

{
  perSystem =
    { pkgs, loom, ... }:
    let
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
    in
    {
      apps = {
        test = {
          type = "app";
          program = "${smokeApp}/bin/test";
          meta.description = "Container smoke harness (Linux only; Darwin stub)";
        };
        fuzz-loom = {
          type = "app";
          program = "${fuzzApp}/bin/fuzz-loom";
          meta.description = "On-demand cargo fuzz runner (not gated by flake check)";
        };
      };
    };
}

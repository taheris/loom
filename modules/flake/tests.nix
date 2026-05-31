# modules/flake/tests.nix
#
# Lifts the `loom gate verify` derivation into `packages.<system>` so
# it can be built directly via `nix build .#loom-tests`. It is *not*
# attached to `checks.<system>` — per specs/pre-commit.md the fast
# tier (`nix flake check`) must not compile the workspace under test;
# that work runs as per-hook prek entries against the host's warm
# cargo cache.
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
    in
    {
      packages.loom-tests = testsDeriv.rustChecks.loom-tests;
    };
}

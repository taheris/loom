# modules/flake/tests.nix
#
# Lifts the derivations exposed by tests/default.nix into the flake's
# `checks.<system>` and `packages.<system>` sets:
#
#   checks.<system>.tests   — the `loom gate verify` run gated by
#                             `nix flake check` on every supported
#                             system (x86_64-linux, aarch64-linux,
#                             x86_64-darwin, aarch64-darwin).
#   packages.<system>.loom-tests — same derivation lifted to packages
#                             so it can be built directly via
#                             `nix build .#loom-tests`.
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
      checks.tests = testsDeriv.rustChecks.loom-tests;
      packages.loom-tests = testsDeriv.rustChecks.loom-tests;
    };
}

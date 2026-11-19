# nix/flake/tests.nix
#
# Exposes the complete workspace-test producer in `packages.<system>`.
# Gate dispatches its declared runtime; Darwin's canonical-store reporter is not
# an inner Nix-sandbox execution. It is *not*
# attached to `checks.<system>` — per specs/pre-commit.md the fast
# tier (`nix flake check`) must not expose full workspace clippy/nextest;
# that work runs through the explicit `nix run .#test` full-suite app.
#
# Spec: specs/tests.md § CI integration / Cross-platform.
_:

{
  perSystem =
    {
      pkgs,
      loom,
      smokeProfileManifest,
      smokeSandbox,
      ...
    }:
    let
      testsDeriv = import ../../tests/default.nix {
        inherit
          pkgs
          smokeProfileManifest
          smokeSandbox
          ;
        loomPackage = loom;
      };
    in
    {
      packages.loom-tests = testsDeriv.rustChecks.loom-tests;
    };
}

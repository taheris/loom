# tests/default.nix
#
# Aggregates the derivations produced by tests/loom/default.nix and
# groups them under `rustChecks` for consumers that want the derivation.
# `loom-tests` runs native source analysis and the complete workspace tests; smoke
# is exposed separately as a `nix run .#smoke` app, not a flake check.
#
# Spec: specs/tests.md § Nix Integration / CI integration.
{
  pkgs,
  loomPackage,
  smokeProfileManifest ? null,
  smokePrekHooks ? null,
  smokeSandbox ? null,
  smokeServiceImage ? null,
  wrixLauncher ? null,
  ...
}:

let
  loomDeriv = import ./loom/default.nix {
    inherit
      pkgs
      loomPackage
      smokeProfileManifest
      smokePrekHooks
      smokeSandbox
      smokeServiceImage
      wrixLauncher
      ;
  };
in
{
  rustChecks = {
    loom-tests = loomDeriv.loomTests;
  };
  inherit (loomDeriv)
    loom-smoke
    loomTests
    smokeAcceptance
    test-app-ignores-host-git-signing
    smoke-host-gate
    ;
}
// pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
  inherit (loomDeriv) smoke-git-policy;
}

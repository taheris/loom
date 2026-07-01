# tests/default.nix
#
# Aggregates the derivations produced by tests/loom/default.nix and
# groups them under `rustChecks` for consumers that want the derivation.
# `loom-tests` is the `loom gate verify` derivation; the container smoke
# is exposed separately as a `nix run .#smoke` app, not a flake check.
#
# Spec: specs/tests.md § Nix Integration / CI integration.
{ pkgs, loomPackage, ... }:

let
  loomDeriv = import ./loom/default.nix { inherit pkgs loomPackage; };
in
{
  rustChecks = {
    loom-tests = loomDeriv.loomTests;
  };
  inherit (loomDeriv) loomTests loom-smoke;
}

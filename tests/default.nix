# tests/default.nix
#
# Aggregates the derivations produced by tests/loom/default.nix and
# groups them under `rustChecks` for the flake's `checks.<system>` set.
# `loom-tests` is the `loom gate verify` derivation; the container smoke
# is exposed separately as a `nix run .#test` app, not a flake check.
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

{ lib }:

let
  inherit (builtins)
    attrNames
    functionArgs
    mapAttrs
    ;
  module = import ../../nix/flake/checks.nix { inputs.nixpkgs.lib = lib; };
  names =
    mapAttrs
      (
        system: _:
        let
          arguments = mapAttrs (
            name: _: throw "native check-name discovery forced execution argument ${name}"
          ) (functionArgs module.perSystem);
        in
        attrNames (module.perSystem (arguments // { inherit system; })).checks
      )
      {
        aarch64-darwin = null;
        aarch64-linux = null;
        x86_64-linux = null;
      };
in
assert names.aarch64-linux == names.x86_64-linux;
assert names.aarch64-darwin != [ ];
assert
  names.aarch64-darwin == builtins.filter (name: name != "nix-pid-only-procfs") names.x86_64-linux;
names

{ pkgs }:

pkgs.prek.overrideAttrs (
  {
    patches ? [ ],
    preUnpack ? "",
    ...
  }:
  {
    patches = patches ++ [ ./patches/prek-hook-status.patch ];
    preUnpack = (import ./cargo-home.nix) + preUnpack;
  }
)

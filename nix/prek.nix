{ pkgs }:

pkgs.prek.overrideAttrs (
  {
    patches ? [ ],
    ...
  }:
  {
    patches = patches ++ [ ./patches/prek-hook-status.patch ];
  }
)

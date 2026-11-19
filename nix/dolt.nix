{ pkgs }:
pkgs.dolt.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [ ./patches/dolt-unix-only.patch ];
})

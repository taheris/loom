{ pkgs }:

pkgs.prek.overrideAttrs (previous: {
  patches = (previous.patches or [ ]) ++ [ ./patches/prek-hook-status.patch ];
})

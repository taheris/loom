# This fixed native-container tool requires kernel restricted proc variants.
# It never retries an unrestricted mount or disables Nix's strict sandbox.
{ pkgs }:
if pkgs.stdenv.hostPlatform.isLinux then
  (pkgs.nix.appendPatches [ ./patches/nix-restricted-proc.patch ]).overrideScope (
    _final: previous: {
      nix-functional-tests = previous.nix-functional-tests.overrideAttrs (old: {
        nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.gzip ];
        postPatch = (old.postPatch or "") + ''
          patchShebangs .
          substituteInPlace flakes/run.sh flakes/config.sh build-remote-input-addressed.sh \
            --replace-fail '#!/bin/sh' '#!${pkgs.bash}/bin/bash'
          substituteInPlace fetchTree-file.sh \
            --replace-fail 'tar cfa test_input.tar.gz inputs' \
              'tar cf - inputs | ${pkgs.gzip}/bin/gzip > test_input.tar.gz'
          substituteInPlace flakes/flakes.sh \
            --replace-fail 'tar cfz "$TEST_ROOT"/flake.tar.gz -C "$TEST_ROOT" flake5' \
              'tar cf - -C "$TEST_ROOT" flake5 | ${pkgs.gzip}/bin/gzip > "$TEST_ROOT"/flake.tar.gz'
          substituteInPlace flakes/build-paths.sh \
            --replace-fail 'tar cfz "$TEST_ROOT"/flake.tar.gz -C "$TEST_ROOT" flake2' \
              'tar cf - -C "$TEST_ROOT" flake2 | ${pkgs.gzip}/bin/gzip > "$TEST_ROOT"/flake.tar.gz'
        '';
      });
    }
  )
else
  pkgs.nix

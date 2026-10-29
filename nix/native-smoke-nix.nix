# Disposable SDK only: no passing result can bypass the real strict Nix build.
{ pkgs }:
let
  nix = import ./native-container-nix.nix { inherit pkgs; };
  restriction =
    pkgs.runCommandCC "smoke-namespace-outage"
      {
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.libseccomp ];
      }
      ''
        mkdir -p "$out/bin"
        $CC -std=c11 -Wall -Wextra -Werror ${../tests/smoke/deny-namespaces.c} \
          $(pkg-config --cflags --libs libseccomp) -o "$out/bin/deny-namespaces"
      '';
  adapter = pkgs.writeShellApplication {
    name = "nix";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.git
    ];
    text = ''
      if [[ "''${1:-}" == build ]]; then
        for argument in "$@"; do
          case "$argument" in
            /nix/store/*-loom-smoke-result.drv^*)
              git_dir=$(git rev-parse --absolute-git-dir)
              if [[ -f "$git_dir/smoke-verifier-unavailable" && ! -f "$git_dir/smoke-verifier-hold" ]]; then
                # A real, additional per-child kernel restriction makes the
                # strict build unavailable. No fabricated Nix diagnostic/verdict.
                exec ${restriction}/bin/deny-namespaces ${nix}/bin/nix "$@"
              fi
              document=$(mktemp "''${TMPDIR:-/tmp}/loom-smoke-build.XXXXXXXX")
              ${nix}/bin/nix "$@" > "$document"
              # Interrupt result delivery only after the real predicate ran.
              ${pkgs.bash}/bin/bash ${../tests/smoke/verifier-probe.sh} loom-smoke-result.txt
              cat "$document"
              exit 0
              ;;
          esac
        done
      fi
      exec ${nix}/bin/nix "$@"
    '';
  };
in
if !pkgs.stdenv.hostPlatform.isLinux then
  nix
else
  pkgs.symlinkJoin {
    name = "loom-smoke-nix-${nix.version}";
    paths = [
      adapter
      nix
    ];
    meta = nix.meta // {
      outputsToInstall = [ "out" ];
    };
    passthru = (nix.passthru or { }) // {
      inherit (nix) version;
    };
  }

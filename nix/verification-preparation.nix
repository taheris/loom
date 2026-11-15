{ pkgs, sdk }:
pkgs.writeShellApplication {
  name = "prepare-verification";
  runtimeInputs = [
    pkgs.nix
    (import ./verification-tool.nix { inherit pkgs; })
    pkgs.coreutils
  ];
  text = ''
    [[ $# == 1 ]] || { echo 'usage: prepare-verification prepare|check' >&2; exit 64; }
    case "$1" in
      prepare) budget=1230 ;;
      check) budget=65 ;;
      *) echo 'usage: prepare-verification prepare|check' >&2; exit 64 ;;
    esac
    exec timeout --kill-after=5s "''${budget}s" loom-verification prepare \
      ${pkgs.lib.escapeShellArg (builtins.unsafeDiscardStringContext sdk.drvPath)} \
      ${pkgs.lib.escapeShellArg (builtins.unsafeDiscardStringContext (toString sdk))} "$1"
  '';
}

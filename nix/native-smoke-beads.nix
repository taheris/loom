# This disposable SDK always asks the admitted workspace broker for fresh host
# credentials, including integration clones without a local Dolt data marker.
# Reuse upstream's credential clearing, scoped sync parser and container behavior.
{ pkgs, upstream }:
upstream.overrideAttrs (
  old:
  let
    guard = ''if [[ -n "$root" && ( -f "$root/.beads/metadata.json" || -f "$root/.beads/dolt/WRIX-MANAGED" ) ]]; then'';
  in
  assert builtins.length (pkgs.lib.splitString guard old.text) == 2;
  {
    text = builtins.replaceStrings [ guard ] [ ''if [[ -n "$root" ]]; then'' ] old.text;
  }
)

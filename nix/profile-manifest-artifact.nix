# This is Loom's export, not metadata inherited by projects using its library.
{ pkgs, manifest }:
let
  checked = import ./verification.nix;
in
checked.artifact {
  runtime = "native-process";
  resources = pkgs.lib.optionals (builtins.elem "rust" manifest.profiles) (
    map
      (
        origin:
        checked.resource {
          root = ../.;
          inherit origin;
          name = builtins.baseNameOf origin;
        }
      )
      [
        "scripts/worker-native-preflight.sh"
        "scripts/configure-cargo-patches.sh"
      ]
  );
  build = _: manifest;
}

# Retain program-constructor provenance while checking the artifact, never running the app.
pkgs:
{
  package,
  name,
  meta ? { },
}:
let
  checked = (import ./verification.nix).app {
    inherit pkgs name;
    package = (import ./native-package.nix) package;
  };
in
checked // { meta = meta // checked.meta; }

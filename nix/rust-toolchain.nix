# Read the existing pinned manifest without executing a derivation during evaluation.
{
  fenix,
  file,
  sha256,
}:
let
  inherit (builtins)
    fromTOML
    readFile
    ;
  toolchain = (fromTOML (readFile file)).toolchain;
  manifestPath = builtins.fetchurl {
    url = "https://static.rust-lang.org/dist/channel-rust-${toolchain.channel}.toml";
    inherit sha256;
  };
  manifest = fromTOML (readFile manifestPath);
  selected = fenix.fromManifest manifest;
in
assert !(toolchain ? path) && !(toolchain ? targets);
(selected.withComponents (
  builtins.filter (component: builtins.hasAttr component selected) (
    manifest.profiles.${toolchain.profile or "default"} ++ toolchain.components or [ ]
  )
))
// {
  inherit manifestPath;
}

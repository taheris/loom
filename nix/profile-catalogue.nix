{ mkProfileManifest }:

{ pkgs, wrixLib, ... }@args:
let
  inherit (builtins)
    mapAttrs
    toJSON
    toString
    unsafeDiscardOutputDependency
    unsafeDiscardStringContext
    ;
  legacy = mkProfileManifest args;
  images = mapAttrs (_: variants: mapAttrs (_: sandbox: sandbox.image) variants) legacy.sandboxes;
  producer = wrixLib.mkProfileCatalogue images;
  provisions = mapAttrs (
    profile: variants:
    mapAttrs (
      runtime: selected:
      pkgs.runCommand "loom-selected-profile.json" { nativeBuildInputs = [ pkgs.jq ]; } ''
        jq --arg profile ${pkgs.lib.escapeShellArg profile} \
          --arg runtime ${pkgs.lib.escapeShellArg runtime} \
          --argjson native '${if profile == "rust" then "\"rust\"" else "null"}' \
          '.[$profile][$runtime].native_preflight = $native' ${selected} > "$out"
      ''
    ) variants
  ) producer.provisions;
  catalogue = {
    schema = 1;
    profiles = mapAttrs (
      profile: variants:
      mapAttrs (
        runtime: entry:
        entry
        // {
          realization = {
            path = unsafeDiscardStringContext (toString provisions.${profile}.${runtime});
            derivation = unsafeDiscardOutputDependency provisions.${profile}.${runtime}.drvPath;
          };
        }
      ) variants
    ) producer.catalogue.profiles;
  };
in
if (wrixLib.capabilities.profileCatalogue or null) != 1 then
  throw "Loom selected provisioning requires Wrix profile catalogue v1; update the Wrix input"
else
  pkgs.writeTextFile {
    name = "loom-profile-catalogue.json";
    text = toJSON catalogue;
    passthru = {
      inherit catalogue provisions;
      inherit (legacy) sandboxes profiles runtimes;
    };
  }

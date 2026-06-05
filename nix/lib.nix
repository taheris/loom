# Pure Nix functions exposed via `loom.lib.*`. No flake-parts knowledge — also
# callable from non-flake contexts via `import ./nix/lib.nix`.

let
  inherit (builtins) mapAttrs;
  workspaceBuilder = import ./workspace.nix;
in
{
  # Build the loom Cargo workspace. Returns `{ bin, clippy, nextest,
  # cargoArtifacts, craneLib, toolchain }`. See `nix/workspace.nix`.
  mkLoom = workspaceBuilder;

  # Build a `profile-images.json` derivation for the given profile set,
  # suitable for `export LOOM_PROFILES_MANIFEST=...`. `profiles` is an attrset
  # of wrapix profile definitions; default covers the three wrapix ships out
  # of the box. `loomBin` is the loom binary to thread into every sandbox's
  # package set — required, so the in-container PATH always carries `loom`.
  mkProfileManifest =
    {
      pkgs,
      wrapixLib,
      loomBin,
      profiles ? { inherit (wrapixLib.profiles) base rust python; },
      agent ? "pi",
      agentPkg ? null,
    }:
    let
      sandboxes = mapAttrs (
        _name: profile:
        wrapixLib.mkSandbox (
          {
            inherit profile;
            inherit agent;
            packages = [ loomBin ];
          }
          // pkgs.lib.optionalAttrs (agentPkg != null) { inherit agentPkg; }
        )
      ) profiles;
      images = mapAttrs (_: s: s.image) sandboxes;
      baseManifest = wrapixLib.mkProfileImages images;
      manifest = mapAttrs (
        name: entry:
        entry
        // pkgs.lib.optionalAttrs (images.${name} ? digest) {
          digest = "${images.${name}.digest}";
        }
      ) baseManifest.passthru.manifest;
    in
    pkgs.writeTextFile {
      name = "profile-images.json";
      text = builtins.toJSON manifest;
      passthru = { inherit manifest; };
    };

  # Wrap the raw loom binary with the matching sandbox launcher defaults, so a
  # consumer only needs the wrapped binary on PATH to run loom end-to-end.
  # Consumers can still override either env var.
  mkLoomBin =
    {
      pkgs,
      loomBuild,
      wrapixLauncher,
      profileManifest,
    }:
    pkgs.runCommand "loom"
      {
        nativeBuildInputs = [ pkgs.makeWrapper ];
        inherit (loomBuild.bin) meta;
      }
      ''
        mkdir -p $out/bin
        makeWrapper ${loomBuild.bin}/bin/loom $out/bin/loom \
          --prefix PATH : ${wrapixLauncher}/bin \
          --set-default LOOM_WRAPIX_BIN ${wrapixLauncher}/bin/wrapix \
          --set-default LOOM_PROFILES_MANIFEST ${profileManifest}
      '';
}

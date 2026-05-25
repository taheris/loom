# Pure Nix functions exposed via `loom.lib.*`. No flake-parts knowledge — also
# callable from non-flake contexts via `import ./nix/lib.nix`.

let
  workspaceBuilder = import ./workspace.nix;
in
{
  # Build the loom Cargo workspace. Returns `{ bin, clippy, nextest,
  # cargoArtifacts, craneLib, toolchain }`. See `nix/workspace.nix`.
  mkLoom = workspaceBuilder;

  # Build a `profile-images.json` derivation for the given profile set,
  # suitable for `export LOOM_PROFILES_MANIFEST=...`. `profiles` is an attrset
  # of wrapix profile definitions; default covers the three wrapix ships out
  # of the box.
  mkProfileManifest =
    {
      wrapixLib,
      profiles ? { inherit (wrapixLib.profiles) base rust python; },
    }:
    let
      sandboxes = builtins.mapAttrs (
        _: profile: wrapixLib.mkSandbox { inherit profile; }
      ) profiles;
      images = builtins.mapAttrs (_: s: s.image) sandboxes;
    in
    wrapixLib.mkProfileImages images;

  # Wrap the raw loom binary with `bin/wrapix` on its internal PATH and
  # `--set-default` LOOM_PROFILES_MANIFEST, so a consumer only needs the
  # wrapped binary on PATH to run `loom plan` end-to-end. Consumers can still
  # override the env var to point at a custom manifest.
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
          --set-default LOOM_PROFILES_MANIFEST ${profileManifest}
      '';
}

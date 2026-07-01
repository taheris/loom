_:

{
  perSystem =
    {
      pkgs,
      sandbox,
      debugSandbox,
      loom,
      loomBin,
      patchedWrixSrc,
      profileManifest,
      ...
    }:
    {
      packages = {
        inherit profileManifest;
        profile-images = profileManifest;

        default = loom.bin;
        loom = loom.bin;
        loom-wrix = loomBin;

        debug = debugSandbox.package;
        sandbox = sandbox.package;
        sandbox-image = sandbox.image;

        wrixSrc = pkgs.runCommand "wrix-src" { } ''
          cp -r ${patchedWrixSrc} $out
        '';
      };
    };
}

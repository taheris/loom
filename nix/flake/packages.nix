_:

{
  perSystem =
    {
      pkgs,
      sandbox,
      debugSandbox,
      imagePiCodingAgent,
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
        pi-coding-agent = imagePiCodingAgent;

        debug = debugSandbox.package;
        sandbox = sandbox.package;
        sandbox-image = sandbox.image;

        wrixSrc = pkgs.runCommand "wrix-src" { } ''
          cp -r ${patchedWrixSrc} $out
        '';
      };
    };
}

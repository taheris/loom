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
      wrixSrc,
      profileManifest,
      ...
    }:
    {
      packages = {
        inherit profileManifest;
        profile-images = profileManifest;

        cargo-patches = loom.cargoConfig;
        default = loom.bin;
        loom = loom.bin;
        loom-wrix = loomBin;
        pi-coding-agent = imagePiCodingAgent;

        debug = debugSandbox.package;
        sandbox = sandbox.package;
        sandbox-image = sandbox.image;

        wrixSrc = pkgs.runCommand "wrix-src" { } ''
          cp -r ${wrixSrc} $out
        '';
      };
    };
}

_:

{
  perSystem =
    {
      sandbox,
      debugSandbox,
      loomBin,
      profileManifest,
      ...
    }:
    {
      packages = {
        default = sandbox.package;
        sandbox = sandbox.package;
        debug = debugSandbox.package;
        loom = loomBin;
        inherit profileManifest;
      };
    };
}

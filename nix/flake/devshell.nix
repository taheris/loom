_:

{
  perSystem =
    {
      config,
      pkgs,
      wrapixLib,
      rustProfile,
      profileManifest,
      sandbox,
      loomBin,
      ...
    }:
    {
      devShells.default = wrapixLib.mkDevShell {
        profile = rustProfile;

        env = {
          LOOM_PROFILES_MANIFEST = profileManifest;
        };

        packages = [
          config.treefmt.build.wrapper
          loomBin
          pkgs.cargo-nextest
          sandbox.package
        ];
      };
    };
}

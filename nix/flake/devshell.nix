_:

{
  perSystem =
    {
      config,
      pkgs,
      wrapixLib,
      rustProfile,
      profileManifest,
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
          pkgs.cargo-nextest
        ];
      };
    };
}

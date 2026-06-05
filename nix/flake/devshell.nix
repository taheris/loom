_:

{
  perSystem =
    {
      config,
      pkgs,
      wrapixLib,
      rustProfile,
      sandbox,
      profileManifest,
      ...
    }:
    {
      devShells.default = wrapixLib.mkDevShell {
        profile = rustProfile;

        env = {
          LOOM_PROFILES_MANIFEST = profileManifest;
          LOOM_WRAPIX_BIN = "${sandbox.package}/bin/wrapix";
        };

        # Keep the same sandbox wrapper on PATH that loom loop gets via
        # LOOM_WRAPIX_BIN, so interactive wrapix runs match the packaged CLI.
        packages = [
          config.treefmt.build.wrapper
          sandbox.package
          pkgs.cargo-nextest
        ];
      };
    };
}

_:

{
  perSystem =
    {
      config,
      pkgs,
      inputs',
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

        # `inputs'.wrapix.packages.sandbox` ships the wrapix launcher CLI
        # without depending on `loom.bin`, so `wrapix` stays on PATH for
        # `loom loop` to spawn agents while direnv reload stays fast.
        packages = [
          config.treefmt.build.wrapper
          inputs'.wrapix.packages.sandbox
          pkgs.cargo-nextest
        ];
      };
    };
}

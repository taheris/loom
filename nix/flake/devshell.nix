_:

{
  perSystem =
    {
      config,
      pkgs,
      wrixLib,
      rustProfile,
      rustToolchain,
      sandbox,
      loom,
      profileManifest,
      ...
    }:
    {
      devShells = {
        default = wrixLib.mkDevShell {
          profile = rustProfile;

          env = {
            LOOM_PROFILES_MANIFEST = profileManifest;
            LOOM_WRIX_BIN = "${sandbox.package}/bin/wrix";
          };

          packages = [
            config.treefmt.build.wrapper
            loom.bin
            pkgs.cargo-nextest
            rustToolchain
            sandbox.package
          ];
        };
      };
    };
}

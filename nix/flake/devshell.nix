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
        default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.cargo-nextest
            config.treefmt.build.wrapper
            loom.bin
          ];
        };

        wrix = wrixLib.mkDevShell {
          profile = rustProfile;

          env = {
            LOOM_PROFILES_MANIFEST = profileManifest;
            LOOM_WRIX_BIN = "${sandbox.package}/bin/wrix";
          };

          # Keep the same sandbox wrapper on PATH that loom loop gets via
          # LOOM_WRIX_BIN, so interactive wrix runs match the packaged CLI.
          packages = [
            config.treefmt.build.wrapper
            sandbox.package
          ];
        };
      };
    };
}

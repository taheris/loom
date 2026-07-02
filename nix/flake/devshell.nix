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
    let
      commonPackages = [
        config.treefmt.build.wrapper
        loom.bin
        pkgs.cargo-nextest
        rustToolchain
        sandbox.package
      ];

      commonEnv = {
        LOOM_PROFILES_MANIFEST = profileManifest;
        LOOM_WRIX_BIN = "${sandbox.package}/bin/wrix";
      };
    in
    {
      devShells = {
        default = wrixLib.mkDevShell {
          profile = rustProfile;
          env = commonEnv;
          packages = commonPackages;
        };

        # Hook-free shell used by CI/system checks. The default wrix devShell
        # starts workspace services for interactive use; `nix run .#test` only
        # needs to prove the Loom binary is present in a Nix development shell
        # and must not prompt for host SSH/deploy-key credentials.
        ci = pkgs.mkShell {
          packages = [ loom.bin ];
        };
      };
    };
}

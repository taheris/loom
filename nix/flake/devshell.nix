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
      inherit (pkgs.lib) optionals;

      wrixSpawnRuntimePackages = [
        pkgs.nix
      ]
      ++ optionals pkgs.stdenv.hostPlatform.isLinux [
        pkgs.podman
        pkgs.skopeo
      ]
      ++ optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.skopeo ];

      commonPackages = [
        config.treefmt.build.wrapper
        loom.bin
        pkgs.cargo-nextest
        rustToolchain
        sandbox.package
      ]
      ++ wrixSpawnRuntimePackages;

      commonEnv = {
        LOOM_PROFILES_MANIFEST = profileManifest;
        LOOM_WRIX_BIN = "${sandbox.package}/bin/wrix";
        LOOM_WRIX_SPAWN_BIN = "${sandbox.launcher}/bin/wrix";
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

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
        pkgs.flock
        rustToolchain
        sandbox.package
      ]
      ++ wrixSpawnRuntimePackages;

      cargoPatchSetup = ''
        if [[ -f .cargo/config.toml ]]; then
          ${pkgs.bash}/bin/bash ${../../scripts/configure-cargo-patches.sh} ${loom.cargoConfig}
        fi
      '';

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
          shellHook = cargoPatchSetup;
        };

        # Hook-free shell used by CI/system checks. The default wrix devShell
        # starts workspace services for interactive use; `nix run .#test` only
        # needs to prove the Loom binary is present in a Nix development shell
        # and must not prompt for host SSH/deploy-key credentials.
        ci = pkgs.mkShell {
          packages = [ loom.bin ];
          shellHook = cargoPatchSetup;
        };
      };
    };
}

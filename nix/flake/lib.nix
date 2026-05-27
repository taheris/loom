{ inputs, ... }:

let
  loomLib = import ../lib.nix;
  loomSrc = ../..;
in
{
  flake.lib = loomLib // {
    # Default crane/fenix/src to loom's own pinned inputs so consumers
    # following them get a one-liner. Pass explicit args to override.
    mkLoom =
      {
        pkgs,
        crane ? inputs.crane,
        fenix ? inputs.fenix,
        toolchain ? null,
        src ? loomSrc,
      }:
      loomLib.mkLoom {
        inherit
          pkgs
          crane
          fenix
          toolchain
          src
          ;
      };

    # Build per-profile container images with loom bundled in by default.
    # `pkgs` is required so loom can be built from loom's own flake inputs
    # and so the rust profile image can carry flock/prek on PATH; pass
    # `loomBin` to override with a specific build.
    mkProfileManifest =
      {
        pkgs,
        wrapixLib,
        profiles ? { inherit (wrapixLib.profiles) base rust python; },
        loomBin ?
          (loomLib.mkLoom {
            inherit
              pkgs
              crane
              fenix
              toolchain
              src
              ;
          }).bin,
        crane ? inputs.crane,
        fenix ? inputs.fenix,
        toolchain ? null,
        src ? loomSrc,
      }:
      loomLib.mkProfileManifest {
        inherit
          pkgs
          wrapixLib
          profiles
          loomBin
          ;
      };
  };

  perSystem =
    {
      inputs',
      pkgs,
      ...
    }:
    let
      wrapixLib = inputs'.wrapix.legacyPackages.lib;

      # The same file + hash pin the toolchain for the wrapix sandbox
      # profile, the loom workspace build, and the devshell.
      rustToolchainFile = ../../rust-toolchain.toml;
      rustToolchainSha256 = "sha256-gh/xTkxKHL4eiRXzWv8KP7vfjSk61Iq48x47BEDFgfk=";

      rustToolchain = inputs'.fenix.packages.fromToolchainFile {
        file = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };

      rustProfile = wrapixLib.rustProfile {
        toolchain = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };

      loom = loomLib.mkLoom {
        inherit pkgs;
        inherit (inputs) crane fenix;
        toolchain = rustToolchain;
        src = loomSrc;
      };

      sandbox = wrapixLib.mkSandbox {
        profile = rustProfile;
        packages = [
          loom.bin
          pkgs.cargo-nextest
        ];
      };

      debugSandbox = wrapixLib.mkSandbox {
        profile = rustProfile;
        packages = [
          loom.bin
          pkgs.cargo-nextest
          pkgs.podman
        ];
      };

      profileManifest = loomLib.mkProfileManifest {
        inherit pkgs wrapixLib;
        loomBin = loom.bin;
      };

      loomBin = loomLib.mkLoomBin {
        inherit pkgs profileManifest;
        loomBuild = loom;
        wrapixLauncher = sandbox.package;
      };
    in
    {
      _module.args = {
        inherit
          debugSandbox
          loom
          loomBin
          profileManifest
          rustProfile
          rustToolchain
          sandbox
          wrapixLib
          ;
      };
    };
}

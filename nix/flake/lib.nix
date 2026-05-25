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
        inherit pkgs crane fenix toolchain src;
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

      rustProfile = wrapixLib.profiles.rust.withToolchain {
        file = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };

      sandbox = wrapixLib.mkSandbox { profile = rustProfile; };

      debugSandbox = wrapixLib.mkSandbox {
        profile = rustProfile;
        packages = [ pkgs.podman ];
      };

      profileManifest = loomLib.mkProfileManifest { inherit wrapixLib; };

      loom = loomLib.mkLoom {
        inherit pkgs;
        inherit (inputs) crane fenix;
        toolchain = rustToolchain;
        src = loomSrc;
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

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
        wrixLib,
        profiles ? { inherit (wrixLib.profiles) base rust python; },
        agent ? "pi",
        agentPkg ? null,
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
          wrixLib
          profiles
          agent
          agentPkg
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
      wrixLib = inputs'.wrix.legacyPackages.lib;
      piCodingAgent = pkgs.pi-coding-agent;

      # The same file + hash pin the toolchain for the wrix sandbox
      # profile, the loom workspace build, and the devshell.
      rustToolchainFile = ../../rust-toolchain.toml;
      rustToolchainSha256 = "sha256-mvUGEOHYJpn3ikC5hckneuGixaC+yGrkMM/liDIDgoU=";

      rustToolchain = inputs'.fenix.packages.fromToolchainFile {
        file = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };

      rustProfile = wrixLib.rustProfile {
        toolchain = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };

      loom = loomLib.mkLoom {
        inherit pkgs;
        inherit (inputs) crane fenix;
        toolchain = rustToolchain;
        src = loomSrc;
      };

      sandbox = wrixLib.mkSandbox {
        profile = rustProfile;
        agent = "pi";
        agentPkg = piCodingAgent;
        packages = [
          loom.bin
        ];
      };

      debugSandbox = wrixLib.mkSandbox {
        profile = rustProfile;
        agent = "pi";
        agentPkg = piCodingAgent;
        packages = [
          loom.bin
          pkgs.podman
        ];
      };

      profileManifest = loomLib.mkProfileManifest {
        inherit pkgs wrixLib;
        loomBin = loom.bin;
        agentPkg = piCodingAgent;
      };

      loomBin = loomLib.mkLoomBin {
        inherit pkgs profileManifest;
        loomBuild = loom;
        wrixLauncher = sandbox.package;
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
          wrixLib
          ;
      };
    };
}

{ inputs, ... }:

let
  loomLib = import ../lib.nix;
  loomSrc = ../..;
  workerProfile = import ../worker-profile.nix;
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
        extraPackages ? [ ],
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
          extraPackages
          loomBin
          ;
      };
  };

  perSystem =
    {
      config,
      inputs',
      pkgs,
      system,
      ...
    }:
    let
      inherit (inputs) nixpkgs;

      linuxSystem =
        if system == "aarch64-darwin" then
          "aarch64-linux"
        else if system == "x86_64-darwin" then
          "x86_64-linux"
        else
          system;

      wrixPkgs = import nixpkgs {
        inherit system;
        config.allowUnfree = true;
      };

      wrixLinuxPkgs = import nixpkgs {
        system = linuxSystem;
        config.allowUnfree = true;
      };

      patchedWrixSrc = pkgs.applyPatches {
        name = "wrix-src-loom-agent";
        src = inputs.wrix;
        patches = [ ../patches/wrix-claude-permission-prompt.patch ];
      };
      wrixLib = import "${patchedWrixSrc}/lib" {
        inherit system;
        inherit (inputs) crane fenix;
        pkgs = wrixPkgs;
        linuxPkgs = wrixLinuxPkgs;
        treefmt = config.treefmt.build.wrapper;
      };
      piCodingAgent = pkgs.pi-coding-agent;
      smokeMockPi = pkgs.writeShellScriptBin "pi" ''
        export MOCK_PI_SCENARIO=happy-path
        export LOOM_SMOKE_WORKER=1
        exec ${pkgs.bash}/bin/bash ${../../tests/mock-pi/pi.sh} "$@"
      '';

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
      workerRustProfile = workerProfile rustProfile;

      loom = loomLib.mkLoom {
        inherit pkgs;
        inherit (inputs) crane fenix;
        toolchain = rustToolchain;
        src = loomSrc;
      };

      sandbox = wrixLib.mkSandbox {
        profile = workerRustProfile;
        agent = "pi";
        agentPkg = piCodingAgent;
        packages = [ loom.bin ];
      };

      debugSandbox = wrixLib.mkSandbox {
        profile = workerRustProfile;
        agent = "pi";
        agentPkg = piCodingAgent;
        packages = [
          loom.bin
          pkgs.podman
        ];
      };

      smokeSandbox = wrixLib.mkSandbox {
        profile = workerProfile wrixLib.profiles.base;
        agent = "pi";
        agentPkg = smokeMockPi;
        packages = [ loom.bin ];
      };

      smokeProfileManifest = wrixLib.mkProfileImages { base = smokeSandbox.image; };

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
          patchedWrixSrc
          profileManifest
          rustProfile
          rustToolchain
          sandbox
          smokeProfileManifest
          smokeSandbox
          wrixLib
          ;
        smokeServiceImage = wrixLib.serviceImage;
      };
    };
}

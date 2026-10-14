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
        imagePkgs ?
          if pkgs.stdenv.hostPlatform.isLinux then
            pkgs
          else
            import inputs.nixpkgs {
              system = "${pkgs.stdenv.hostPlatform.parsed.cpu.name}-linux";
              config.allowUnfree = true;
            },
        wrixLib,
        profiles ? { inherit (wrixLib.profiles) base rust python; },
        agent ? "pi",
        agentPkg ? null,
        extraPackages ? [ ],
        nativeCargo ? null,
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
          imagePkgs
          wrixLib
          profiles
          agent
          agentPkg
          extraPackages
          nativeCargo
          loomBin
          ;
      };
  };

  perSystem =
    {
      inputs',
      mkTreefmtConfig,
      pkgs,
      system,
      ...
    }:
    let
      inherit (inputs) nixpkgs;
      nextestOverlay = import ../nextest-overlay.nix;

      linuxSystem =
        if system == "aarch64-darwin" then
          "aarch64-linux"
        else if system == "x86_64-darwin" then
          "x86_64-linux"
        else
          system;

      wrixPkgs = import nixpkgs {
        inherit system;
        overlays = [
          nextestOverlay
          (_final: _prev: {
            beads = inputs.wrix.packages.${system}.beads;
          })
        ];
        config.allowUnfree = true;
      };

      wrixLinuxPkgs = import nixpkgs {
        system = linuxSystem;
        overlays = [ nextestOverlay ];
        config.allowUnfree = true;
      };

      imageRustToolchain = inputs.fenix.packages.${linuxSystem}.fromToolchainFile {
        file = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };
      imageTreefmtWrapper = inputs.treefmt-nix.lib.mkWrapper wrixLinuxPkgs (
        mkTreefmtConfig imageRustToolchain
      );

      patchedWrixSrc = pkgs.applyPatches {
        name = "wrix-src-loom-agent";
        src = inputs.wrix;
        patches = [
          ../patches/wrix-claude-permission-prompt.patch
          ../patches/wrix-git-helper-bash.patch
          ../patches/wrix-known-hosts-store.patch
          ../patches/wrix-nix-state-ancestors.patch
        ];
      };
      wrixLib = import "${patchedWrixSrc}/lib" {
        inherit system;
        inherit (inputs) crane fenix;
        pkgs = wrixPkgs;
        linuxPkgs = wrixLinuxPkgs;
        treefmt = imageTreefmtWrapper;
      };
      imagePiCodingAgent = import "${patchedWrixSrc}/lib/sandbox/pi.nix" {
        pkgs = wrixLinuxPkgs;
      };
      smokeMockPi = wrixLinuxPkgs.writeShellScriptBin "pi" ''
        set -euo pipefail
        export MOCK_PI_SCENARIO=happy-path
        export LOOM_SMOKE_WORKER=1
        exec ${wrixLinuxPkgs.bash}/bin/bash ${../../tests/mock-pi/pi.sh} "$@"
      '';

      # The same file + hash pin the toolchain for the wrix sandbox
      # profile, the loom workspace build, and the devshell.
      rustToolchainFile = ../../rust-toolchain.toml;
      rustToolchainSha256 = "sha256-zm3dyIY2T414ZRR3EhLOvptzG6gta4WZUcawzMUWtqI=";

      rustToolchain = inputs'.fenix.packages.fromToolchainFile {
        file = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };

      rustProfile = wrixLib.rustProfile {
        toolchain = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };
      nativeCargo = import ../native-cargo.nix {
        pkgs = wrixLinuxPkgs;
        config = imageLoom.cargoConfig;
        toolchain = imageLoom.toolchain;
      };
      workerRustProfile = workerProfile (
        rustProfile // { packages = [ nativeCargo ] ++ rustProfile.packages; }
      );

      loom = loomLib.mkLoom {
        inherit pkgs;
        inherit (inputs) crane fenix;
        toolchain = rustToolchain;
        src = loomSrc;
      };

      imageLoom = loomLib.mkLoom {
        pkgs = wrixLinuxPkgs;
        inherit (inputs) crane fenix;
        toolchain = imageRustToolchain;
        src = loomSrc;
      };

      sandbox = wrixLib.mkSandbox {
        profile = workerRustProfile;
        agent = "pi";
        agentPkg = imagePiCodingAgent;
        packages = [ imageLoom.bin ];
      };

      debugSandbox = wrixLib.mkSandbox {
        profile = workerRustProfile;
        agent = "pi";
        agentPkg = imagePiCodingAgent;
        packages = [
          imageLoom.bin
          wrixLinuxPkgs.podman
        ];
      };

      smokeSandbox = wrixLib.mkSandbox {
        profile = workerProfile wrixLib.profiles.base;
        agent = "pi";
        agentPkg = smokeMockPi;
        packages = [ imageLoom.bin ];
      };

      smokeProfileManifest = wrixLib.mkProfileImages { base = smokeSandbox.image; };

      profileManifest = loomLib.mkProfileManifest {
        inherit pkgs wrixLib nativeCargo;
        imagePkgs = wrixLinuxPkgs;
        profiles = {
          inherit (wrixLib.profiles) base python;
          rust = rustProfile;
        };
        loomBin = imageLoom.bin;
        agentPkg = imagePiCodingAgent;
      };

      loomBin = loomLib.mkLoomBin {
        inherit pkgs profileManifest;
        loomBuild = loom;
        wrixLauncher = sandbox.package;
      };
    in
    {
      _module.args = {
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ nextestOverlay ];
          config.allowUnfree = true;
        };
        inherit
          debugSandbox
          imagePiCodingAgent
          imageTreefmtWrapper
          loom
          loomBin
          nativeCargo
          patchedWrixSrc
          profileManifest
          rustProfile
          rustToolchain
          sandbox
          smokeProfileManifest
          smokeSandbox
          wrixLib
          wrixLinuxPkgs
          ;
        smokeServiceImage = wrixLib.serviceImage;
      };
    };
}

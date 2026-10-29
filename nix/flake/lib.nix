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
      linuxSystem = if system == "aarch64-darwin" then "aarch64-linux" else system;

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

      imageRustToolchain = import ../rust-toolchain.nix {
        fenix = inputs.fenix.packages.${linuxSystem};
        file = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };
      imageTreefmtWrapper = inputs.treefmt-nix.lib.mkWrapper wrixLinuxPkgs (
        mkTreefmtConfig imageRustToolchain
      );

      cargoGitSources = [
        {
          url = "https://github.com/celler-cache/celler.git";
          source = "git+https://github.com/celler-cache/celler.git?rev=${inputs.celler-src.rev}#${inputs.celler-src.rev}";
          input = inputs.celler-src;
        }
        {
          url = "https://codeberg.org/blitz/gorgon.git";
          source = "git+https://codeberg.org/blitz/gorgon.git?branch=nix-daemon-2#${inputs.gorgon-src.rev}";
          input = inputs.gorgon-src;
        }
      ];
      wrixSrc = inputs.wrix;
      wrixModules = import ../wrix-runtime-source.nix {
        inherit pkgs;
        src = wrixSrc;
        cargoGitHashes = builtins.listToAttrs (
          map (dependency: {
            name = dependency.source;
            value = dependency.input.narHash;
          }) cargoGitSources
        );
      };
      wrixLib = wrixModules.library {
        inherit system;
        inherit (inputs) crane fenix;
        pkgs = wrixPkgs;
        linuxPkgs = wrixLinuxPkgs;
        treefmt = imageTreefmtWrapper;
      };
      imagePiCodingAgent = wrixModules.pi {
        pkgs = wrixLinuxPkgs;
      };
      smokeMockPi = wrixLinuxPkgs.writeShellScriptBin "pi" ''
        set -euo pipefail
        if [[ "''${LOOM_SMOKE_INTERRUPT_ONCE:-0}" == 1 ]]; then
          exec ${wrixLinuxPkgs.bash}/bin/bash ${../../tests/smoke/interruption-pi.sh} "$@"
        fi
        export MOCK_PI_SCENARIO=happy-path
        export LOOM_SMOKE_WORKER=1
        exec ${wrixLinuxPkgs.bash}/bin/bash ${../../tests/mock-pi/pi.sh} "$@"
      '';

      # The same file + hash pin the toolchain for the wrix sandbox
      # profile, the loom workspace build, and the devshell.
      rustToolchainFile = ../../rust-toolchain.toml;
      rustToolchainSha256 = "sha256-+xUr6Al1wZSqX8aEsq7DpuFOK4Ve/i+qTl+K5uWQ9GM=";

      rustToolchain = import ../rust-toolchain.nix {
        fenix = inputs'.fenix.packages;
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
        nativeTestInputs = [
          (import (wrixSrc + "/lib/services/rust.nix") {
            pkgs = wrixPkgs;
            rustProfile = wrixLib.profiles.rust;
          }).wrix
          wrixLib.skipIfMissing
        ];
        nativeTestSources =
          let
            sources =
              flake:
              [ flake.outPath ]
              ++ pkgs.lib.concatMap sources (
                builtins.attrValues (builtins.removeAttrs (flake.inputs or { }) [ "self" ])
              );
          in
          pkgs.lib.unique (
            [
              rustToolchain.manifestPath
              wrixLib.rustPackage.package.cargoVendorDir
            ]
            ++ map (
              dependency:
              wrixPkgs.fetchgit {
                inherit (dependency) url;
                inherit (dependency.input) rev;
                hash = dependency.input.narHash;
                fetchSubmodules = true;
                fetchLFS = true;
              }
            ) cargoGitSources
            ++ pkgs.lib.concatMap sources (builtins.attrValues (builtins.removeAttrs inputs [ "self" ]))
          );
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

      smokeWrixLib = wrixModules.library {
        inherit system;
        inherit (inputs) crane fenix;
        pkgs = wrixPkgs;
        linuxPkgs = wrixLinuxPkgs // {
          nix = import ../native-smoke-nix.nix { pkgs = wrixLinuxPkgs; };
        };
        treefmt = imageTreefmtWrapper;
      };
      smokeSandbox = smokeWrixLib.mkSandbox {
        profile = workerProfile smokeWrixLib.profiles.base;
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

      wrixLauncher = wrixLib.rustPackage.wrix;

      loomBin = loomLib.mkLoomBin {
        inherit pkgs profileManifest wrixLauncher;
        loomBuild = loom;
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
          imageLoom
          imageTreefmtWrapper
          loom
          loomBin
          nativeCargo
          wrixSrc
          profileManifest
          rustProfile
          rustToolchain
          sandbox
          smokeProfileManifest
          smokeSandbox
          wrixLib
          wrixLauncher
          wrixModules
          wrixLinuxPkgs
          ;
        smokeServiceImage = wrixLib.serviceImage;
      };
    };
}

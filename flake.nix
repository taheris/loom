{
  description = "Loom — Rust workflow orchestrator for spec-driven AI development";

  inputs = {
    nixpkgs.url = "git+ssh://git@github.com/NixOS/nixpkgs.git?ref=nixos-unstable&shallow=1";

    wrapix = {
      url = "git+ssh://git@github.com/taheris/wrapix.git?ref=main&shallow=1";
      inputs = {
        flake-parts.follows = "flake-parts";
        nixpkgs.follows = "nixpkgs";
      };
    };

    crane.follows = "wrapix/crane";
    fenix.follows = "wrapix/fenix";

    flake-parts = {
      url = "git+ssh://git@github.com/hercules-ci/flake-parts.git?ref=main&shallow=1";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };

    treefmt-nix = {
      url = "git+https://github.com/numtide/treefmt-nix.git?ref=main&shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ inputs.treefmt-nix.flakeModule ];

      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      # `mkLoom` is the public-contract build function. Consumers that need a
      # Linux-targeted `loom-direct-runner` to drop into a wrapix sandbox
      # call it with their own `linuxPkgs` (and inherited crane/fenix).
      flake.lib.mkLoom =
        {
          pkgs,
          crane ? inputs.crane,
          fenix ? inputs.fenix,
        }:
        import ./nix/loom.nix {
          inherit pkgs crane fenix;
          src = ./.;
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
          loom = import ./nix/loom.nix {
            inherit pkgs;
            inherit (inputs) crane fenix;
            src = ./.;
          };

          wrapixLib = inputs'.wrapix.legacyPackages.lib;
          rustProfile = wrapixLib.profiles.rust;

          sandbox = wrapixLib.mkSandbox {
            profile = rustProfile;
          };

          debugSandbox = wrapixLib.mkSandbox {
            profile = rustProfile;
            packages = [ pkgs.podman ];
          };

          devToolchain =
            let
              fenixPkgs = inputs.fenix.packages.${system};
            in
            fenixPkgs.combine [
              fenixPkgs.stable.defaultToolchain
              fenixPkgs.stable.rust-analyzer-preview
              fenixPkgs.stable.rust-src
            ];
        in
        {
          packages = {
            default = sandbox.package;
            sandbox = sandbox.package;
            debug = debugSandbox.package;
            loom = loom.bin;
          };

          checks = {
            inherit (loom) bin clippy nextest;
          };

          devShells.default = wrapixLib.mkDevShell {
            shellHook = ''
              export PATH="${devToolchain}/bin:$PATH"
              export RUSTC_WRAPPER="${pkgs.sccache}/bin/sccache"
              export SCCACHE_DIR="''${SCCACHE_DIR:-$HOME/.cache/sccache}"
              export SCCACHE_CACHE_SIZE="''${SCCACHE_CACHE_SIZE:-50G}"
              export CARGO_INCREMENTAL="''${CARGO_INCREMENTAL:-0}"
            '';

            packages = [
              devToolchain
              pkgs.sccache
              config.treefmt.build.wrapper
              pkgs.cargo-nextest
            ];
          };

          treefmt = {
            projectRootFile = "flake.nix";
            programs.nixfmt.enable = true;
            programs.rustfmt = {
              enable = true;
              package = inputs.fenix.packages.${system}.stable.rustfmt;
            };
            programs.shellcheck.enable = true;
          };
        };
    };
}

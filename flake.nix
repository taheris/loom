{
  description = "Loom — Rust workflow orchestrator for spec-driven AI development";

  inputs = {
    nixpkgs.url = "git+ssh://git@github.com/NixOS/nixpkgs.git?ref=nixos-unstable&shallow=1";

    crane = {
      url = "git+https://github.com/ipetkov/crane.git?ref=master&shallow=1";
    };

    fenix = {
      url = "git+https://github.com/nix-community/fenix.git?ref=main&shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };

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
        { pkgs, system, ... }:
        let
          loom = import ./nix/loom.nix {
            inherit pkgs;
            inherit (inputs) crane fenix;
            src = ./.;
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
          packages.default = loom.bin;
          packages.loom = loom.bin;

          checks = {
            inherit (loom) bin clippy nextest;
          };

          devShells.default = pkgs.mkShell {
            packages = [
              devToolchain
              pkgs.cargo-nextest
              pkgs.git
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

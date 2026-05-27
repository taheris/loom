{
  description = "Loom — workflow orchestrator for spec-driven AI development";

  inputs = {
    nixpkgs.url = "git+https://github.com/NixOS/nixpkgs.git?ref=nixos-unstable&shallow=1";

    wrapix = {
      url = "git+https://github.com/taheris/wrapix.git?ref=main&shallow=1";
      inputs = {
        flake-parts.follows = "flake-parts";
        nixpkgs.follows = "nixpkgs";
      };
    };

    crane.follows = "wrapix/crane";
    fenix.follows = "wrapix/fenix";

    flake-parts = {
      url = "git+https://github.com/hercules-ci/flake-parts.git?ref=main&shallow=1";
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
      imports = [
        inputs.treefmt-nix.flakeModule
        ./nix/flake/lib.nix
        ./nix/flake/packages.nix
        ./nix/flake/checks.nix
        ./nix/flake/devshell.nix
        ./nix/flake/formatter.nix
        ./modules/flake/tests.nix
        ./modules/flake/apps.nix
        ./modules/flake/overlays.nix
      ];

      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
    };
}

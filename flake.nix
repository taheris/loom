{
  description = "Loom — workflow orchestrator for spec-driven AI development";

  inputs = {
    nixpkgs.url = "git+https://github.com/NixOS/nixpkgs.git?ref=nixpkgs-unstable&shallow=1";

    wrix = {
      url = "git+https://github.com/taheris/wrix.git?ref=main&shallow=1";
      inputs = {
        flake-parts.follows = "flake-parts";
        nixpkgs.follows = "nixpkgs";
      };
    };

    crane.follows = "wrix/crane";
    fenix.follows = "wrix/fenix";

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
    let
      # Preserve each native definition while binding the complete shared evaluator
      # context once at the namespace composition boundary, never as runtime access.
      nativeOutputs = builtins.mapAttrs (
        _system: builtins.mapAttrs (_name: import ./nix/native-package.nix)
      );
      flake = flake-parts.lib.mkFlake { inherit inputs; } {
        imports = [
          inputs.treefmt-nix.flakeModule
          ./nix/flake/lib.nix
          ./nix/flake/packages.nix
          ./nix/flake/checks.nix
          ./nix/flake/devshell.nix
          ./nix/flake/formatter.nix
          ./nix/flake/tests.nix
          ./nix/flake/apps.nix
          ./nix/flake/quint.nix
          ./nix/flake/system-verification.nix
          ./nix/flake/cargo-retention.nix
        ];

        systems = [
          "aarch64-darwin"
          "aarch64-linux"
          "x86_64-linux"
        ];
      };
    in
    flake
    // {
      checks = nativeOutputs flake.checks;
      packages = nativeOutputs flake.packages;
    };
}

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

    celler-src = {
      url = "git+https://github.com/celler-cache/celler.git?rev=de7e34848770f3cd274a276e15d3c459fee86f99&submodules=1";
      flake = false;
    };
    gorgon-src = {
      url = "git+https://codeberg.org/blitz/gorgon.git?ref=nix-daemon-2&rev=f42f79a9d52787802ccaf23cf67329d32a78184c&submodules=1";
      flake = false;
    };

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

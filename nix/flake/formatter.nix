_:

{
  perSystem =
    { rustToolchain, ... }:
    {
      treefmt = {
        projectRootFile = "flake.nix";
        programs.nixfmt.enable = true;
        programs.rustfmt = {
          enable = true;
          package = rustToolchain;
        };
        programs.shellcheck.enable = true;
      };
    };
}

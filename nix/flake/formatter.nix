_:

let
  mkTreefmtConfig = rustToolchain: {
    projectRootFile = "flake.nix";
    programs.nixfmt.enable = true;
    programs.rustfmt = {
      enable = true;
      package = rustToolchain;
    };
    programs.shellcheck = {
      enable = true;
      excludes = [ ".envrc" ];
    };
  };
in
{
  perSystem =
    { rustToolchain, ... }:
    {
      treefmt = mkTreefmtConfig rustToolchain;
      _module.args = { inherit mkTreefmtConfig; };
    };
}

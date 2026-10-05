_:

let
  mkTreefmtConfig = rustToolchain: {
    projectRootFile = "flake.nix";
    programs.nixfmt.enable = true;
    programs.prettier = {
      enable = true;
      includes = [ "*.md" ];
      settings = {
        embeddedLanguageFormatting = "off";
        printWidth = 80;
        proseWrap = "always";
        overrides = [
          {
            # Prettier resolves these against its generated config in the Nix store.
            files = [
              "*(../)**/crates/loom-templates/templates/**"
              "*(../)**/tests/fixtures/planning_prompt_interview_modes.md"
            ];
            # Runtime text keeps its line boundaries, but still receives Markdown formatting.
            options.proseWrap = "preserve";
          }
        ];
      };
    };
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

# Import immutable upstream modules without derivation-backed evaluator inputs.
{ pkgs, src }:
let
  inherit (builtins) scopedImport toString;
  inherit (pkgs.lib) hasPrefix hasSuffix;
  root = toString src;

  # Keep Fenix's own file/dir, profile, component and cross-target semantics.
  # Only its fixed-hash channel manifest fetch uses the evaluation primitive;
  # compiler components remain ordinary derivations with the original packages.
  pinnedFenix =
    fenix:
    fenix
    // {
      packages = builtins.mapAttrs (
        system: packages:
        let
          basePkgs = fenix.inputs.nixpkgs.legacyPackages.${system};
          pure = import fenix.outPath {
            inherit system;
            inherit (fenix.inputs.nixpkgs) lib;
            rust-analyzer-src = fenix.inputs.rust-analyzer-src;
            pkgs = basePkgs // {
              fetchurl =
                args:
                if
                  builtins.match "https://static\\.rust-lang\\.org/dist/(.*/)?channel-rust-.*\\.toml" args.url != null
                then
                  builtins.fetchurl { inherit (args) url sha256; }
                else
                  basePkgs.fetchurl args;
            };
          };
        in
        packages // { inherit (pure) fromToolchainFile; }
      ) fenix.packages;
    };

  load =
    path:
    let
      filename =
        if hasSuffix ".nix" (toString path) then toString path else "${toString path}/default.nix";
      original = scopedImport { import = load; } filename;
    in
    if !hasPrefix "${root}/" filename then
      import path
    else if filename == "${root}/lib/default.nix" then
      { fenix, ... }@args: original (args // { fenix = pinnedFenix fenix; })
    else
      original;
in
{
  library = load "${root}/lib";
  pi = load "${root}/lib/sandbox/pi.nix";
}

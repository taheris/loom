# Import immutable upstream modules without derivation-backed evaluator inputs.
{
  pkgs,
  src,
  cargoGitHashes ? { },
}:
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

  pinnedCrane =
    crane:
    crane
    // {
      mkLib =
        packages:
        (crane.mkLib packages).overrideScope (
          _final: previous: {
            vendorCargoDeps =
              args:
              previous.vendorCargoDeps (
                args
                // {
                  outputHashes = cargoGitHashes // (args.outputHashes or { });
                }
              );
          }
        );
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
      { fenix, crane, ... }@args:
      original (
        args
        // {
          fenix = pinnedFenix fenix;
          crane = pinnedCrane crane;
        }
      )
    else if filename == "${root}/lib/services/rust.nix" then
      { rustProfile, ... }@args:
      original (
        args
        // {
          rustProfile = rustProfile // {
            buildPackage =
              buildArgs:
              let
                workspace = rustProfile.buildPackage buildArgs;
                patched =
                  derivation:
                  derivation.overrideAttrs (old: {
                    patches = (old.patches or [ ]) ++ [ ./patches/wrix-storage-options.patch ];
                  });
              in
              workspace
              // {
                bin = patched workspace.bin;
                clippy = patched workspace.clippy;
                nextest = patched workspace.nextest;
              };
          };
        }
      )
    else
      original;
in
{
  library = load "${root}/lib";
  pi = load "${root}/lib/sandbox/pi.nix";
}

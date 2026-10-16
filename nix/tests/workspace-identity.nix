{
  pkgs,
  crane,
  fenix,
  loom,
  src,
}:
let
  inherit (pkgs.lib) cleanSourceWith;
  construct =
    overrides:
    import ../workspace.nix (
      {
        inherit
          pkgs
          crane
          fenix
          src
          ;
        inherit (loom) toolchain;
      }
      // overrides
    );
  without =
    relative:
    cleanSourceWith {
      inherit src;
      filter = path: _: path != "${toString src}/${relative}";
    };
  baseline = construct { };
  readme = construct { src = without "README.md"; };
  docs = construct { src = without "docs/README.md"; };
  rust = construct { src = without "crates/loom-gate/src/provider/tracked.rs"; };
  manifest = construct { src = without "crates/loom-events/Cargo.toml"; };
  toolchain = construct {
    toolchain = loom.toolchain.overrideAttrs (old: {
      name = "${old.name}-identity-control";
    });
  };
  patch = construct {
    crane = crane // {
      mkLib =
        packages:
        (crane.mkLib packages).overrideScope (
          _: previous: {
            downloadCargoPackage =
              args:
              (previous.downloadCargoPackage args).overrideAttrs (_: {
                LOOM_PATCH_IDENTITY_CONTROL = "changed";
              });
          }
        );
    };
  };
  identity =
    workspace:
    map (value: value.drvPath) [
      workspace.cargoArtifacts
      workspace.bin
      workspace.stagedSrc
      workspace.clippy
      workspace.nextest
    ];
in
assert baseline.bin.drvPath == loom.bin.drvPath;
assert identity baseline == identity readme;
assert baseline.cargoArtifacts.drvPath == rust.cargoArtifacts.drvPath;
assert baseline.bin.drvPath != rust.bin.drvPath;
assert baseline.stagedSrc.drvPath != rust.stagedSrc.drvPath;
assert baseline.cargoArtifacts.drvPath == docs.cargoArtifacts.drvPath;
assert baseline.bin.drvPath == docs.bin.drvPath;
assert baseline.stagedSrc.drvPath != docs.stagedSrc.drvPath;
assert
  builtins.readFile baseline.cargoArtifacts.cargoLock == builtins.readFile (src + "/Cargo.lock");
assert baseline.cargoArtifacts.drvPath != manifest.cargoArtifacts.drvPath;
assert baseline.cargoArtifacts.drvPath != toolchain.cargoArtifacts.drvPath;
assert baseline.cargoArtifacts.drvPath != patch.cargoArtifacts.drvPath;
assert baseline.cargoConfig != patch.cargoConfig;
pkgs.runCommand "workspace-build-identity" { } ''
  touch "$out"
''

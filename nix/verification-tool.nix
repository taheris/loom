{ pkgs }:
let
  inherit (pkgs.lib) hasPrefix removePrefix;
  checked = import ./verification.nix;
  root = ../.;
  workspaceLock = (builtins.fromTOML (builtins.readFile ../Cargo.lock)).package;
  bootstrapLock = (builtins.fromTOML (builtins.readFile ./verification-tool.lock)).package;
  identity = package: {
    inherit (package) name version;
    source = package.source or null;
    checksum = package.checksum or null;
  };
  source = checked.resource {
    inherit root;
    name = "loom-verification-source";
    filter =
      path: type:
      let
        relative = removePrefix "${toString root}/" (toString path);
      in
      if type == "directory" then
        toString path == toString root
        || relative == "crates"
        || hasPrefix "crates/loom-verification" relative
      else
        builtins.elem relative [
          "Cargo.toml"
          "Cargo.lock"
          "clippy.toml"
        ]
        || hasPrefix "crates/loom-verification/" relative;
  };
in
assert pkgs.lib.all (
  package: builtins.elem (identity package) (map identity workspaceLock)
) bootstrapLock;
checked.artifact {
  runtime = "native-process";
  resources = [ source ];
  evaluation = import ./native-context.nix;
  build =
    _:
    pkgs.rustPlatform.buildRustPackage {
      pname = "loom-verification";
      version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).workspace.package.version;
      src = source.path;
      cargoLock.lockFile = ./verification-tool.lock;
      postPatch = ''
        sed -i '/^members = \[/,/^]/c\members = ["crates/loom-verification"]' Cargo.toml
        cp ${./verification-tool.lock} Cargo.lock
      '';
      meta.mainProgram = "loom-verification";
      cargoBuildFlags = [
        "-p"
        "loom-verification"
      ];
      cargoTestFlags = [
        "-p"
        "loom-verification"
      ];
      strictDeps = true;
      nativeBuildInputs = [
        pkgs.jq
        pkgs.makeWrapper
      ];
      doCheck = false;
      preBuild = ''
        export CARGO_HOME
        CARGO_HOME=$(mktemp -d)
      '';
      postBuild = ''
        cargo test --offline --locked --release --no-run -p loom-verification \
          --target ${pkgs.stdenv.hostPlatform.rust.rustcTarget} --message-format=json > verification-test-artifacts.json
      '';
      postInstall = ''
        wrapProgram "$out/bin/loom-verification" --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.coreutils ]}
        mkdir -p "$out/libexec"
        jq -er 'select(.reason == "compiler-artifact" and .profile.test and .target.name == "loom-verification") | .executable | select(. != null)' \
          verification-test-artifacts.json > verification-test-binaries
        [[ $(wc -l < verification-test-binaries) == 1 ]]
        cp "$(<verification-test-binaries)" "$out/libexec/loom-verification-tests"
        jq -er 'select(.reason == "compiler-artifact" and .profile.test and .target.name == "cli") | .executable | select(. != null)' \
          verification-test-artifacts.json > verification-cli-binaries
        [[ $(wc -l < verification-cli-binaries) == 1 ]]
        cp "$(<verification-cli-binaries)" "$out/libexec/loom-verification-cli-tests"
      '';
    };
}

# Dependency-free actual Rust tests, with the same pinned compiler/Nextest as Loom.
{
  pkgs,
  crane,
  toolchain,
  nextest,
}:
let
  files = {
    "flake.nix" = ''
      {
        inputs.nixpkgs.url = "path:${pkgs.path}";
        inputs.crane.url = "path:${crane}";
        outputs = { self, nixpkgs, crane, ... }: let
          pkgs = import nixpkgs { system = "${pkgs.stdenv.hostPlatform.system}"; };
          # Preserve the original valid store closures, not source-copy symlinks
          # whose runtime dependencies would be absent inside the build sandbox.
          compilerPath = builtins.appendContext "${toolchain}" { "${toolchain}" = { path = true; }; };
          nextestPath = builtins.appendContext "${nextest}" { "${nextest}" = { path = true; }; };
          craneLib = (crane.mkLib pkgs).overrideToolchain compilerPath;
          checked = import ./verification.nix;
          source = checked.resource {
            origin = ".";
            root = ./.;
            filter = path: type: type == "directory" ||
              builtins.any (suffix: pkgs.lib.hasSuffix suffix path) [ ".rs" ".toml" ".lock" ".txt" ];
          };
          construct = operation: checked.cargo {
            inherit craneLib operation;
            resources = [ source ];
            evaluation = map (origin: checked.resource { root = ./.; inherit origin; }) [ "flake.nix" "flake.lock" "verification.nix" "report-supervisor.sh" "private-fixture-tests.sh" ];
            args = {
              src = source.path;
              cargoArtifacts = null;
              cargoVendorDir = craneLib.vendorCargoDeps {
                cargoLock = builtins.path { path = ./Cargo.lock; name = "Cargo.lock"; };
              };
              nativeBuildInputs = [ nextestPath ];
            };
          };
        in {
          packages.${pkgs.stdenv.hostPlatform.system} = {
            native-build = construct "build";
            native-lint = construct "lint";
            native-lint-all = construct "lint-all-targets";
            native-tests = construct "test";
          };
        };
      }
    '';
    "Cargo.toml" = ''
      [package]
      name = "native-probe"
      version = "0.1.0"
      edition = "2024"
      [[package.metadata.loom.verify.resource]]
      name = "fixture:payload"
      directory = "payload"
      role = "test-fixture"
      consumers = ["cargo:native-probe:lib:native_probe"]
    '';
    "Cargo.lock" = ''
      version = 4
      [[package]]
      name = "native-probe"
      version = "0.1.0"
    '';
    "src/lib.rs" = ''
      #[cfg(test)]
      mod tests {
          #[test]
          fn native_case() {
              assert_eq!(include_str!("../payload/input.txt"), "valid\n");
          }
          #[test]
          fn other_case() {
              assert_eq!(std::hint::black_box(2) + 2, 4);
          }
          #[test]
          #[ignore]
          fn ignored_case() {
              panic!("an ignored case cannot authorize a named binding");
          }
          #[test] fn unmatched_case() { assert_eq!(std::hint::black_box(3), 3); }
          mod a { #[test] fn same_case() { assert_eq!(std::hint::black_box(1), 1); } }
          mod b { #[test] fn same_case() { assert_eq!(std::hint::black_box(1), 1); } }
      }
    '';
    ".config/nextest.toml" = ''
      [profile.default]
      default-filter = 'test(other_case)'
      [test-groups.probe]
      max-threads = 1
      [[profile.default.overrides]]
      filter = 'test(native_case)'
      test-group = 'probe'
    '';
    "payload/input.txt" = "valid\n";
    "loom.toml" = "";
    ".gitignore" = "/.loom/\n/target/\n";
    "docs/README.md" = "- [Probe](../specs/probe/spec.md)\n";
    "specs/probe/spec.md" = "# Native Rust probe\n";
    "specs/probe/tests.md" =
      "# Native Rust probe\n\n```loom-verify\n[rust]\nproperty_filters = ['group(probe)', 'default()', 'test(same_case)']\n```\n\n## Success Criteria\n- The actual compiled case passes [test](native_case)\n";
  };
in
pkgs.runCommand "loom-native-rust-fixture"
  {
    nativeBuildInputs = [
      pkgs.nix
      pkgs.jq
    ];
  }
  (
    ''mkdir -p "$out"''
    + "\n"
    + pkgs.lib.concatStringsSep "\n" (
      pkgs.lib.mapAttrsToList (name: body: ''
        mkdir -p "$out/$(dirname ${pkgs.lib.escapeShellArg name})"
        cp ${pkgs.writeText (builtins.baseNameOf name) body} "$out/${name}"
      '') files
    )
    + ''
      cp ${../verification.nix} "$out/verification.nix"
      cp ${../report-supervisor.sh} "$out/report-supervisor.sh"
      cp ${../private-fixture-tests.sh} "$out/private-fixture-tests.sh"
      # Hash declared immutable input bytes during fixture construction, not on
      # every emulated guest lock. Nix still resolves and verifies the lock normally.
      nixpkgsHash=$(nix --extra-experimental-features nix-command hash path --sri --type sha256 ${pkgs.path})
      craneHash=$(nix --extra-experimental-features nix-command hash path --sri --type sha256 ${crane})
      jq -n --arg nixpkgs ${pkgs.lib.escapeShellArg "${pkgs.path}"} --arg crane ${pkgs.lib.escapeShellArg (toString crane)} \
        --arg nixpkgsHash "$nixpkgsHash" --arg craneHash "$craneHash" '
        {version: 7, root: "root", nodes: {
          root: {inputs: {nixpkgs: "nixpkgs", crane: "crane"}},
          nixpkgs: {original: {type: "path", path: $nixpkgs}, locked: {type: "path", path: $nixpkgs, narHash: $nixpkgsHash}},
          crane: {original: {type: "path", path: $crane}, locked: {type: "path", path: $crane, narHash: $craneHash}}
        }}' > "$out/flake.lock"
    ''
  )

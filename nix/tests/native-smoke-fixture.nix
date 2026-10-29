# The shipped smoke predicate reads only its declared immutable result, never .git state.
{ pkgs }:
let
  checked = import ../verification.nix;
  resources =
    map
      (
        origin:
        checked.resource {
          root = ../..;
          inherit origin;
        }
      )
      [
        "nix/verification.nix"
        "tests/smoke/result-check.sh"
        "tests/smoke/spec.md"
        "bin/pre-push-checks"
      ];
in
checked.check {
  inherit resources;
  evaluation = import ../native-context.nix;
  build =
    sources:
    pkgs.runCommand "loom-native-smoke-fixture" { } ''
      mkdir -p "$out/docs" "$out/specs" "$out/bin"
      cp ${builtins.elemAt sources 3} "$out/bin/pre-push-checks"
      cp ${builtins.elemAt sources 0} "$out/verification.nix"
      cp ${builtins.elemAt sources 1} "$out/result-check.sh"
      cp ${builtins.elemAt sources 2} "$out/specs/smoke.md"
      substituteInPlace "$out/specs/smoke.md" --replace-fail '.#checks.x86_64-linux.' '.#checks.${pkgs.stdenv.hostPlatform.system}.'
      printf -- '- [smoke](../specs/smoke.md)\n' > "$out/docs/README.md"
      printf '/.loom/\n/target/\n' > "$out/.gitignore"
      printf 'not implemented\n' > "$out/loom-smoke-result.txt"
      cat > "$out/flake.nix" <<'FLAKE'
      {
        outputs = { self }: let
          checked = import ./verification.nix;
          result = checked.resource { root = ./.; origin = "loom-smoke-result.txt"; };
          program = checked.resource { root = ./.; origin = "result-check.sh"; };
          hook = checked.resource { root = ./.; origin = "bin/pre-push-checks"; };
        in {
          checks.${pkgs.stdenv.hostPlatform.system}.smoke-result = checked.check {
            resources = [ result program hook ];
            evaluation = map (origin: checked.resource { root = ./.; inherit origin; }) [ "flake.nix" "verification.nix" ];
            build = sources: builtins.derivation {
              name = "loom-smoke-result";
              system = "${pkgs.stdenv.hostPlatform.system}";
              builder = builtins.appendContext "${pkgs.bash}/bin/bash" { "${pkgs.bash}" = { path = true; }; };
              PATH = builtins.appendContext "${pkgs.bash}/bin:${pkgs.git}/bin:${pkgs.coreutils}/bin" {
                "${pkgs.bash}" = { path = true; };
                "${pkgs.git}" = { path = true; };
                "${pkgs.coreutils}" = { path = true; };
              };
              args = [ (builtins.elemAt sources 1) (builtins.elemAt sources 0) (builtins.elemAt sources 2) ];
            };
          };
        };
      }
      FLAKE
    '';
}

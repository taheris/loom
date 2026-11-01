{ pkgs, runner }:
let
  tools = [
    pkgs.bash
    (pkgs.lib.getBin pkgs.nix)
    pkgs.git
    pkgs.coreutils
  ];
  registration = pkgs.closureInfo { rootPaths = tools; };
in
pkgs.runCommand "native-test-environment"
  {
    nativeBuildInputs = tools;
    LOOM_TEST_NIX_REGISTRATION = "${registration}/registration";
  }
  ''
    export PATH=${pkgs.lib.makeBinPath tools}
    export HOME=$(mktemp -d)
    export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
    mkdir source
    cd source
    cat > flake.nix <<'NIX'
    {
      outputs = { self }: {
        checks.${pkgs.stdenv.hostPlatform.system}.actual = builtins.derivation {
          name = "actual-private-native-check";
          system = "${pkgs.stdenv.hostPlatform.system}";
          builder = builtins.appendContext "${pkgs.bash}/bin/bash" {
          "${pkgs.bash}" = { path = true; };
        };
          args = [ "-c" "echo actual > $out" ];
        };
      };
    }
    NIX
    bash ${runner} --build-campaign \
      nix build --offline --no-substitute --no-link \
        --option sandbox true --option sandbox-fallback false \
        --option sandbox-paths "" --option extra-sandbox-paths "" \
        --option builders "" .#checks.${pkgs.stdenv.hostPlatform.system}.actual
    touch "$out"
  ''

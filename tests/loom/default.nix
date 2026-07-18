# tests/loom/default.nix
#
# Builds the test entry points consumed by the flake:
#
#   loomTests — deterministic `loom gate` tiers driven by
#               `craneLib.mkCargoDerivation`. It runs explicit
#               `loom gate check --tree` and `loom gate test --tree`
#               commands inside the Nix build sandbox. The `[system]`
#               tier is excluded because its verifiers shell out to
#               `nix run` / `podman`, neither of which is available
#               inside the Nix build sandbox.
#
#   test-app-ignores-host-git-signing — regression check that executes
#               the full-suite script against a host signing config.
#
#   loom-smoke — Linux-only `writeShellApplication` wrapping the
#               container smoke harness (`tests/run-tests.sh`). On
#               Darwin a stub script exits 0 with the documented
#               "container smoke not available on Darwin" message so
#               `nix run .#smoke` is a no-op rather than an error.
#
# Spec: specs/tests.md § Nix Integration / Cross-platform / CI integration.
{ pkgs, loomPackage, ... }:

let
  inherit (pkgs) lib;
  inherit (lib) optionalAttrs;
  isLinux = pkgs.stdenv.hostPlatform.isLinux;
  inherit (loomPackage)
    craneLib
    stagedSrc
    cargoArtifacts
    bin
    ;

  loomTests = craneLib.mkCargoDerivation {
    pname = "tests";
    version = "0.0.0";
    src = stagedSrc;
    inherit cargoArtifacts;
    doCheck = true;
    nativeBuildInputs = [
      pkgs.git
      pkgs.cargo-nextest
      pkgs.cacert
      bin
    ];
    buildPhaseCargoCommand = ''
      cargo --version
      cargo nextest --version
      loom --version
    '';
    # genai builds a reqwest TLS client eagerly; sandbox needs a CA bundle.
    preCheck = ''
      export HOME=$(mktemp -d)
      export GIT_CONFIG_GLOBAL="$PWD/tests/fixtures/git/test-gitconfig"
      export GIT_CONFIG_SYSTEM=/dev/null
      export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
    '';
    # `--tree` is the explicit scope for each deterministic tier in a
    # git-less build sandbox: the source artifact has no `.git`, so a
    # `--diff`-based scope can't resolve and `loom gate` fails loudly
    # rather than silently degrading. Running `check` and `test` separately
    # excludes `[system]` without an environment override.
    checkPhaseCargoCommand = ''
      loom gate check --tree
      loom gate test --tree
    '';
  };

  fakeFullTestGit = pkgs.writeShellScriptBin "git" ''
    set -euo pipefail

    if [[ "$*" != "rev-parse --show-toplevel" ]]; then
      printf 'unexpected git invocation: %s\n' "$*" >&2
      exit 1
    fi
    printf '%s\n' "$TEST_REPO_ROOT"
  '';

  fakeFullTestNix = pkgs.writeShellScriptBin "nix" ''
    set -euo pipefail

    signing=$(${pkgs.git}/bin/git config --get commit.gpgsign)
    if [[ "$signing" != "false" ]]; then
      printf 'nix inherited host commit.gpgsign=%s\n' "$signing" >&2
      exit 1
    fi
    if [[ "$GIT_CONFIG_GLOBAL" != "$TEST_REPO_ROOT/tests/fixtures/git/test-gitconfig" ]]; then
      printf 'nix received unexpected GIT_CONFIG_GLOBAL=%s\n' "$GIT_CONFIG_GLOBAL" >&2
      exit 1
    fi
    if [[ "$GIT_CONFIG_SYSTEM" != "/dev/null" ]]; then
      printf 'nix received unexpected GIT_CONFIG_SYSTEM=%s\n' "$GIT_CONFIG_SYSTEM" >&2
      exit 1
    fi
    if [[ -n "''${WRIX_SIGNING_KEY+x}" ]]; then
      printf 'nix inherited WRIX_SIGNING_KEY\n' >&2
      exit 1
    fi
    touch "$TEST_APP_OBSERVED"
    exit 99
  '';

  fakeFullTestPath = pkgs.buildEnv {
    name = "full-test-fake-path";
    paths = [
      fakeFullTestGit
      fakeFullTestNix
    ];
    pathsToLink = [ "/bin" ];
  };

  test-app-ignores-host-git-signing = pkgs.runCommand "test-app-ignores-host-git-signing" { } ''
    set -euo pipefail

    export HOME="$TMPDIR/home"
    export TEST_REPO_ROOT="$TMPDIR/repo"
    export TEST_APP_OBSERVED="$TMPDIR/observed"
    mkdir -p "$HOME" "$TEST_REPO_ROOT/tests/fixtures/git"
    cp ${../fixtures/git/test-gitconfig} "$TEST_REPO_ROOT/tests/fixtures/git/test-gitconfig"
    cat > "$HOME/.gitconfig" <<'EOF'
    [user]
    signingkey = host-key-that-tests-must-not-use
    [commit]
    gpgsign = true
    EOF
    unset GIT_CONFIG_GLOBAL
    unset GIT_CONFIG_SYSTEM
    export WRIX_SIGNING_KEY="$TMPDIR/host-signing-key"

    set +e
    PATH="${fakeFullTestPath}/bin:$PATH" bash ${../../scripts/full-test.sh}
    rc=$?
    set -e
    if [[ "$rc" -ne 99 || ! -e "$TEST_APP_OBSERVED" ]]; then
      printf 'full test script did not reach the isolated nix command (exit %s)\n' "$rc" >&2
      exit 1
    fi
    touch "$out"
  '';

  smokeApp = pkgs.writeShellApplication {
    name = "smoke";
    runtimeInputs = [
      bin
      pkgs.podman
      pkgs.jq
      pkgs.git
    ];
    text = builtins.readFile ../run-tests.sh;
  };

  darwinStub = pkgs.writeShellApplication {
    name = "smoke";
    text = ''
      echo "container smoke not available on Darwin"
      exit 0
    '';
  };
in
{
  inherit loomTests test-app-ignores-host-git-signing;
}
// optionalAttrs isLinux { loom-smoke = smokeApp; }
// optionalAttrs (!isLinux) { loom-smoke = darwinStub; }

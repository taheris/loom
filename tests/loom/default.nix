# tests/loom/default.nix
#
# Builds the test entry points consumed by the flake:
#
#   loomTests — registered source walks and the complete workspace Nextest suite.
#               Native producers execute directly, without recursively admitting
#               the project graph that contains this producer. System scenarios
#               remain separately registered native definitions.
#
#   test-app-ignores-host-git-signing — regression check that executes
#               the full-suite script against a host signing config.
#
#   smoke-git-policy — real offline Wrix signing with only smoke Git tools.
#   smoke-host-gate — real host gate and hooks with only smoke runtime tools.
#
#   loom-smoke — Linux-only `writeShellApplication` wrapping the
#               container smoke harness (`tests/run-tests.sh`). On
#               Darwin the unavailable entry exits 69, never a passing smoke.
#
# Spec: specs/tests.md § Nix Integration / Cross-platform / CI integration.
{
  pkgs,
  loomPackage,
  smokeProfileManifest ? null,
  smokePrekHooks ? null,
  smokeSandbox ? null,
  smokeServiceImage ? null,
  wrixLauncher ? null,
  ...
}:

let
  checked = import ../../nix/verification.nix;
  checkResource =
    origin:
    checked.resource {
      root = ../..;
      inherit origin;
    };
  inherit (pkgs) lib;
  inherit (lib) makeBinPath optionalAttrs;
  isLinux = pkgs.stdenv.hostPlatform.isLinux;
  inherit (loomPackage)
    craneLib
    stagedSrc
    cargoArtifacts
    cargoVendorDir
    bin
    ;

  loomTests = craneLib.mkCargoDerivation {
    pname = "tests";
    version = "0.0.0";
    doInstallCargoArtifacts = false;
    src = stagedSrc;
    inherit cargoArtifacts cargoVendorDir;
    doCheck = true;
    nativeBuildInputs = [
      pkgs.git
      pkgs.beads
      pkgs.dolt
      pkgs.cargo-nextest
      pkgs.cacert
      pkgs.flock
      pkgs.jq
      pkgs.openssh
      pkgs.prek
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
    # Native producers must not recursively admit their own project output graph.
    checkPhaseCargoCommand = ''
      bash ${../../scripts/run-private-fixture-tests} cargo run --offline --locked -p loom-walk --bin loom-walk -- --from-specs
      bash ${../../scripts/run-private-fixture-tests} cargo nextest run --offline --locked --workspace --ignore-default-filter --test-threads 2
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

  test-app-ignores-host-git-signing =
    let
      nativeSources = map checkResource [
        "tests/fixtures/git/test-gitconfig"
        "scripts/full-test.sh"
      ];
    in
    checked.check {
      resources = nativeSources;
      evaluation = map checkResource [
        "nix"
        "flake.nix"
        "flake.lock"
        "rust-toolchain.toml"
        "Cargo.toml"
        "Cargo.lock"
        "tests/default.nix"
        "tests/loom/default.nix"
        "scripts/full-test.sh"
        "scripts/required-test.sh"
      ];
      build =
        _:
        pkgs.runCommand "test-app-ignores-host-git-signing" { } ''
          set -euo pipefail

          export HOME="$TMPDIR/home"
          export TEST_REPO_ROOT="$TMPDIR/repo"
          export TEST_APP_OBSERVED="$TMPDIR/observed"
          mkdir -p "$HOME" "$TEST_REPO_ROOT/tests/fixtures/git"
          cp ${(builtins.elemAt nativeSources 0).path} "$TEST_REPO_ROOT/tests/fixtures/git/test-gitconfig"
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
          PATH="${fakeFullTestPath}/bin:$PATH" bash ${(builtins.elemAt nativeSources 1).path}
          rc=$?
          set -e
          if [[ "$rc" -ne 99 || ! -e "$TEST_APP_OBSERVED" ]]; then
            printf 'full test script did not reach the isolated nix command (exit %s)\n' "$rc" >&2
            exit 1
          fi
          touch "$out"
        '';
    };

  smokeAcceptance = craneLib.mkCargoDerivation {
    pname = "loom-smoke-task-acceptance";
    version = "0.1.0";
    doInstallCargoArtifacts = false;
    src = stagedSrc;
    inherit cargoArtifacts cargoVendorDir;
    buildPhaseCargoCommand = "cargo build --release -p loom-workflow --example smoke-task-acceptance";
    installPhaseCommand = ''
      install -Dm755 target/release/examples/smoke-task-acceptance "$out/bin/smoke-task-acceptance"
    '';
    doCheck = false;
  };

  smokeEntry = smokeProfileManifest.passthru.manifest.base.pi;

  smokeGitInputs = [
    pkgs.bash
    pkgs.git
    pkgs.openssh
    wrixLauncher
  ];

  smoke-git-policy =
    let
      nativeSources = map checkResource [
        "tests/smoke/git-policy-test.sh"
        "tests/smoke/seed-git-policy.sh"
      ];
    in
    checked.check {
      resources = nativeSources;
      evaluation = map checkResource [
        "nix"
        "flake.nix"
        "flake.lock"
        "rust-toolchain.toml"
        "Cargo.toml"
        "Cargo.lock"
        "tests/default.nix"
        "tests/loom/default.nix"
        "scripts/full-test.sh"
        "scripts/required-test.sh"
      ];
      build =
        _:
        pkgs.runCommand "smoke-git-policy" { } ''
          set -euo pipefail
          env -i \
            PATH="${
              makeBinPath (
                smokeGitInputs
                ++ [
                  pkgs.coreutils
                  pkgs.jq
                ]
              )
            }" \
            TMPDIR="$TMPDIR" \
            ${pkgs.bash}/bin/bash ${(builtins.elemAt nativeSources 0).path} \
              ${wrixLauncher}/bin/wrix ${(builtins.elemAt nativeSources 1).path}
          touch "$out"
        '';
    };

  smokeRuntimeInputs = [
    bin
    pkgs.beads
    pkgs.coreutils
    pkgs.findutils
    pkgs.gnugrep
    pkgs.dolt
    pkgs.jq
    pkgs.nix
    pkgs.podman
    pkgs.prek
    pkgs.skopeo
  ]
  ++ smokeGitInputs;

  smoke-host-gate =
    let
      nativeSources = map checkResource [
        "tests/smoke/host-gate-temporary-root-test.sh"
        "scripts/run-private-fixture-tests"
        "tests/smoke/host-gate-test.sh"
        "bin/pre-push-checks"
      ];
      nativeFixture = import ../../nix/tests/native-smoke-fixture.nix { inherit pkgs; };
      nativeInputClosure = pkgs.closureInfo {
        rootPaths = [
          pkgs.bash
          pkgs.nix
        ];
      };
    in
    checked.check {
      resources = nativeSources;
      evaluation =
        (import ../../nix/native-context.nix)
        ++ loomPackage.testResources
        ++ map checkResource [
          "nix"
          "flake.nix"
          "flake.lock"
          "rust-toolchain.toml"
          "Cargo.toml"
          "Cargo.lock"
          "tests/default.nix"
          "tests/loom/default.nix"
          "scripts/full-test.sh"
          "scripts/required-test.sh"
          "tests/smoke/result-check.sh"
          "tests/smoke/spec.md"
        ];
      build =
        _:
        pkgs.runCommand "smoke-host-gate" { } ''
          set -euo pipefail
          env -i \
            PATH="${
              makeBinPath (
                [
                  bin
                  pkgs.coreutils
                  pkgs.findutils
                  pkgs.gnugrep
                  pkgs.jq
                  pkgs.nix
                  pkgs.prek
                ]
                ++ smokeGitInputs
              )
            }" \
            TMPDIR="$TMPDIR" \
            NIX_BUILD_TOP="''${NIX_BUILD_TOP:-}" \
            ${pkgs.bash}/bin/bash ${(builtins.elemAt nativeSources 1).path} \
              ${pkgs.bash}/bin/bash ${(builtins.elemAt nativeSources 0).path} \
                ${(builtins.elemAt nativeSources 2).path} ${wrixLauncher}/bin/wrix ${(builtins.elemAt nativeSources 3).path} \
              ${nativeFixture} ${nativeInputClosure}/registration
          touch "$out"
        '';
    };

  smokeApp = pkgs.writeShellApplication {
    name = "smoke";
    runtimeInputs = smokeRuntimeInputs;
    text = ''
      export LOOM_TEST_IMAGE=${smokeSandbox.image.source or smokeSandbox.image}
      export LOOM_TEST_IMAGE_REF=${smokeEntry.ref}
      export LOOM_TEST_IMAGE_SOURCE_KIND=${smokeEntry.source_kind}
      export LOOM_TEST_PROFILE_CONFIG=${smokeEntry.profile_config}
      export LOOM_TEST_PRE_PUSH_CHECKS=${../../bin/pre-push-checks}
      export LOOM_TEST_NATIVE_SMOKE_FIXTURE=${
        import ../../nix/tests/native-smoke-fixture.nix { inherit pkgs; }
      }
      export LOOM_TEST_VERIFIER_PROBE=${../smoke/verifier-probe.sh}
      export LOOM_TEST_GIT_POLICY=${../smoke/seed-git-policy.sh}
      export LOOM_TEST_SEED_BEADS=${../smoke/seed-beads.sh}
      export LOOM_TEST_SMOKE_SPEC=${../smoke/spec.md}
      export LOOM_TEST_ACCEPTANCE_HELPER=${smokeAcceptance}/bin/smoke-task-acceptance
      export LOOM_TEST_LOOP=${../smoke/loop.sh}
      export LOOM_TEST_WAIT_DOLT=${../smoke/wait-dolt.sh}
      export WRIX_PREK_HOOKS=${smokePrekHooks}
      export WRIX_SERVICE_IMAGE=${smokeServiceImage.ref}
      export WRIX_SERVICE_IMAGE_SOURCE=${smokeServiceImage.source}
      export WRIX_SERVICE_IMAGE_SOURCE_KIND=${smokeServiceImage.source_kind}
      export WRIX_SERVICE_IMAGE_DIGEST=${smokeServiceImage.digest}
      export LOOM_WRIX_BIN=${wrixLauncher}/bin/wrix
      export LOOM_WRIX_SERVICE_BIN=${wrixLauncher}/bin/wrix
      export LOOM_WRIX_SPAWN_BIN=${wrixLauncher}/bin/wrix
      ${builtins.readFile ../run-tests.sh}
    '';
  };

  darwinStub = pkgs.writeShellApplication {
    name = "smoke";
    text = ''
      echo "container smoke not available on Darwin" >&2
      exit 69
    '';
  };
in
{
  inherit
    loomTests
    test-app-ignores-host-git-signing
    smokeAcceptance
    smoke-host-gate
    ;
}
// optionalAttrs isLinux {
  loom-smoke = smokeApp;
  inherit smoke-git-policy;
}
// optionalAttrs (!isLinux) { loom-smoke = darwinStub; }

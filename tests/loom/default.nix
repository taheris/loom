# tests/loom/default.nix
#
# Builds the test entry points consumed by the flake:
#
#   loomTests — the canonical complete workspace Nextest producer.
#               Registered source walks remain in their dedicated fast check;
#               aliases must not create another whole-suite execution. System
#               scenarios remain separately registered native definitions.
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
  smokeBeads ? pkgs.beads,
  smokeNix ? pkgs.nix,
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

  loomTests = loomPackage.nextest;

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

  smokeProcHook = pkgs.writeShellScript "smoke-restricted-proc" ''
    set -euo pipefail
    exec ${pkgs.jq}/bin/jq -ec '
      if ([.mounts[] | select(.type == "proc" and .destination == "/proc")] | length) != 1
      then error("expected one OCI proc mount")
      else .mounts |= map(
        if .type == "proc" and .destination == "/proc"
        then .options = ((.options // []) + ["subset=pid"] | unique)
        elif .type == "devpts" and .destination == "/dev/pts"
        then .options |= map(if startswith("gid=") then "gid=0" else . end)
        else . end)
      end'
  '';
  smokeProcHooks = pkgs.writeTextDir "restricted-proc.json" (
    builtins.toJSON {
      version = "1.0.0";
      hook = {
        path = toString smokeProcHook;
        args = [ "smoke-restricted-proc" ];
      };
      when.always = true;
      stages = [ "precreate" ];
    }
  );

  smokeRuntimeInputs = [
    bin
    smokeBeads
    pkgs.coreutils
    pkgs.findutils
    pkgs.gnugrep
    pkgs.dolt
    pkgs.jq
    smokeNix
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
        "tests/smoke/workspace-membership.sh"
        "tests/smoke/workspace-membership-test.sh"
      ];
      nativeFixture = import ../../nix/tests/native-smoke-fixture.nix { inherit pkgs; };
      nativeInputClosure = pkgs.closureInfo {
        rootPaths = [
          pkgs.bash
          pkgs.nix
          pkgs.git
          pkgs.coreutils
          nativeFixture
          (builtins.elemAt nativeSources 3).path
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
          original='{"mounts":[{"type":"proc","destination":"/proc","options":["nosuid","nodev","noexec"]},{"type":"tmpfs","destination":"/tmp"},{"type":"devpts","destination":"/dev/pts","options":["nosuid","noexec","newinstance","ptmxmode=0666","mode=0620","gid=5"]}],"linux":{"maskedPaths":["/proc/kcore"],"readonlyPaths":["/proc/sys"]},"process":{"noNewPrivileges":true}}'
          printf '%s\n' "$original" | ${smokeProcHook} > restricted.json
          ${pkgs.jq}/bin/jq -e --argjson original "$original" '
            .mounts[0].options == ["nodev","noexec","nosuid","subset=pid"] and
            .mounts[1] == $original.mounts[1] and
            .mounts[2].options == ["nosuid","noexec","newinstance","ptmxmode=0666","mode=0620","gid=0"] and
            .linux == $original.linux and .process == $original.process
          ' restricted.json
          ${smokeProcHook} < restricted.json > repeated.json
          cmp restricted.json repeated.json
          env -i PATH="${
            makeBinPath [
              pkgs.bash
              pkgs.coreutils
              pkgs.git
              pkgs.jq
            ]
          }" \
            HOME="$TMPDIR" TMPDIR="$TMPDIR" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
            ${pkgs.bash}/bin/bash ${(builtins.elemAt nativeSources 5).path} ${(builtins.elemAt nativeSources 4).path}
          if printf '{"mounts":[]}\n' | ${smokeProcHook}; then
            echo 'restricted proc hook accepted missing OCI proc mount' >&2
            exit 1
          fi
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
      export LOOM_TEST_MEMBERSHIP_SCRIPT=${../smoke/workspace-membership.sh}
      export LOOM_TEST_MEMBERSHIP_PATH=${
        makeBinPath [
          pkgs.bash
          pkgs.coreutils
          pkgs.git
          pkgs.jq
        ]
      }
      export LOOM_TEST_SHELL=${pkgs.bash}/bin/bash
      export LOOM_TEST_PROC_HOOKS=${smokeProcHooks}
      export LOOM_TEST_FUSE_OVERLAYFS=${pkgs.fuse-overlayfs}/bin/fuse-overlayfs
      export LOOM_TEST_IMAGE=${smokeSandbox.image.source or smokeSandbox.image}
      export LOOM_TEST_IMAGE_REF=${smokeEntry.ref}
      export LOOM_TEST_IMAGE_SOURCE_KIND=${smokeEntry.source_kind}
      export LOOM_TEST_PROFILE_CONFIG=${smokeEntry.profile_config}
      export LOOM_TEST_BEADS_BOOTSTRAP=${pkgs.beads}/bin/bd
      export LOOM_TEST_PRE_PUSH_CHECKS=${../../bin/pre-push-checks}
      export LOOM_TEST_NATIVE_SMOKE_FIXTURE=${
        import ../../nix/tests/native-smoke-fixture.nix { inherit pkgs; }
      }
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

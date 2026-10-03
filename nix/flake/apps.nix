# Exposes user-facing `nix run` entry points:
#
# - `.#test`: complete standalone suite.
# - `.#test-required`: full nextest and system coverage after pre-push lint hooks.
# - `.#smoke`: container smoke harness.
#   Linux checks runtime devices before realizing the image-backed runner;
#   Darwin returns a no-op stub.
# - `.#test-sandbox`: checks offline Pi health and the real sandbox hook chain.
#   Skips with exit 77 when the platform cannot run the container runtime.
# - `.#fuzz-loom`: on-demand `cargo fuzz` driver.
#   This is intentionally not gated by `nix flake check`.
#
# Spec: specs/tests.md § CI integration / Cross-platform.
_:

{
  perSystem =
    {
      pkgs,
      loom,
      smokeProfileManifest,
      smokeSandbox,
      smokeServiceImage,
      wrixLib,
      ...
    }:
    let
      inherit (pkgs.stdenv.hostPlatform) isLinux;

      testsDeriv = import ../../tests/default.nix {
        inherit
          pkgs
          smokeProfileManifest
          smokeSandbox
          smokeServiceImage
          ;
        loomPackage = loom;
        smokePrekHooks = wrixLib.prekHooks;
      };

      smokeRuntime = testsDeriv.loom-smoke;

      smokePreflight = pkgs.writeShellApplication {
        name = "smoke";
        runtimeInputs = [ pkgs.nix ];
        text = ''
          exec ${pkgs.bash}/bin/bash ${../../scripts/run-smoke.sh} \
            /dev/fuse /dev/net/tun .#smoke-runtime "$@"
        '';
      };

      smokeApp = if isLinux then smokePreflight else smokeRuntime;

      testRuntimeInputs = [
        pkgs.cargo-nextest
        pkgs.git
        pkgs.nix
        loom.bin
        loom.toolchain
      ];

      testApp = pkgs.writeShellApplication {
        name = "test";
        runtimeInputs = testRuntimeInputs;
        text = builtins.readFile ../../scripts/full-test.sh;
      };

      requiredTestApp = pkgs.writeShellApplication {
        name = "test-required";
        runtimeInputs = testRuntimeInputs;
        text = builtins.readFile ../../scripts/required-test.sh;
      };

      fuzzApp = pkgs.writeShellApplication {
        name = "fuzz-loom";
        runtimeInputs = [
          pkgs.cargo-fuzz
          loom.toolchain
        ];
        text = ''
          if [[ "$#" -eq 0 ]]; then
            echo "usage: nix run .#fuzz-loom -- <fuzz-target> [cargo-fuzz args...]" >&2
            exit 64
          fi
          exec cargo fuzz "$@"
        '';
      };

      sandboxSmokeLinux = pkgs.writeShellApplication {
        name = "test-sandbox";
        runtimeInputs = [
          pkgs.nix
          pkgs.podman
          pkgs.coreutils
          pkgs.gnugrep
          pkgs.gnused
        ];
        text = ''
          export LOOM_SANDBOX_IMAGE_ATTR=".#sandbox-image"
          export LOOM_TEST_SANDBOX_SOURCE=${loom.stagedSrc}
          export WRIX_PREK_HOOKS=${wrixLib.prekHooks}
          ${builtins.readFile ../../scripts/test-sandbox.sh}
        '';
      };

      sandboxSmokeDarwin = pkgs.writeShellApplication {
        name = "test-sandbox";
        text = ''
          echo "test-sandbox not available on Darwin"
          exit 77
        '';
      };

      sandboxSmokeApp = if isLinux then sandboxSmokeLinux else sandboxSmokeDarwin;
    in
    {
      packages.smoke-runtime = smokeRuntime;

      apps = {
        test = {
          type = "app";
          program = "${testApp}/bin/test";
          meta.description = "Full required suite: flake check, clippy, full nextest, and system/container verifiers";
        };
        test-required = {
          type = "app";
          program = "${requiredTestApp}/bin/test-required";
          meta.description = "Full nextest and system/container verifiers";
        };
        smoke = {
          type = "app";
          program = "${smokeApp}/bin/smoke";
          meta.description = "Container smoke harness (Linux only; Darwin stub)";
        };
        test-sandbox = {
          type = "app";
          program = "${sandboxSmokeApp}/bin/test-sandbox";
          meta.description = "Offline agent health and hook-chain checks for the Rust sandbox image (Linux only; Darwin stub)";
        };

        fuzz-loom = {
          type = "app";
          program = "${fuzzApp}/bin/fuzz-loom";
          meta.description = "On-demand cargo fuzz runner (not gated by flake check)";
        };
      };
    };
}

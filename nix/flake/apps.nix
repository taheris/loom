# Exposes user-facing `nix run` entry points:
#
# - `.#test`: full required suite for pre-push and manual verification.
# - `.#smoke`: container smoke harness.
#   Linux runs `tests/run-tests.sh`; Darwin returns a no-op stub.
# - `.#test-sandbox`: boots `.#sandbox` and checks Pi plus agent hook assembly.
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
      };

      smokeApp = testsDeriv.loom-smoke;

      testApp = pkgs.writeShellApplication {
        name = "test";
        runtimeInputs = [
          pkgs.cargo-nextest
          pkgs.git
          pkgs.nix
          loom.bin
          loom.toolchain
        ];
        text = builtins.readFile ../../scripts/full-test.sh;
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
          pkgs.git
          pkgs.jq
          pkgs.nix
          pkgs.podman
        ];
        text = ''
          export LOOM_SANDBOX_IMAGE_ATTR=".#sandbox-image"
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
      apps = {
        test = {
          type = "app";
          program = "${testApp}/bin/test";
          meta.description = "Full required suite: flake check, clippy, full nextest, and system/container verifiers";
        };
        smoke = {
          type = "app";
          program = "${smokeApp}/bin/smoke";
          meta.description = "Container smoke harness (Linux only; Darwin stub)";
        };
        test-sandbox = {
          type = "app";
          program = "${sandboxSmokeApp}/bin/test-sandbox";
          meta.description = "Runtime and agent-hook assembly check for the Rust sandbox image (Linux only; Darwin stub)";
        };

        fuzz-loom = {
          type = "app";
          program = "${fuzzApp}/bin/fuzz-loom";
          meta.description = "On-demand cargo fuzz runner (not gated by flake check)";
        };
      };
    };
}

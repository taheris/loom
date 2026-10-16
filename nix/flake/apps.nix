# Exposes user-facing `nix run` entry points:
#
# - `.#test`: complete standalone suite.
# - `.#test-required`: full nextest and system coverage after pre-push lint hooks.
# - `.#smoke`: container smoke harness.
#   Linux checks runtime devices before realizing the image-backed runner;
#   Darwin returns a no-op stub.
# - `.#test-sandbox`: checks offline Pi health, private Nix builds, and sandbox hooks.
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
      wrixLinuxPkgs,
      profileManifest,
      imageLoom,
      nativeCargo,
      rustProfile,
      sandbox,
      ...
    }:
    let
      inherit (pkgs.stdenv.hostPlatform) isLinux;
      inherit (pkgs.lib) optionals;

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
        pkgs.coreutils
        pkgs.cargo-nextest
        pkgs.git
        pkgs.nix
        loom.bin
        loom.toolchain
      ];

      testApp = pkgs.writeShellApplication {
        name = "test";
        runtimeInputs = testRuntimeInputs;
        runtimeEnv.LOOM_CARGO_PATCHES = loom.cargoConfig;
        text = builtins.readFile ../../scripts/full-test.sh;
      };

      requiredTestApp = pkgs.writeShellApplication {
        name = "test-required";
        runtimeInputs = testRuntimeInputs;
        runtimeEnv.LOOM_CARGO_PATCHES = loom.cargoConfig;
        text = builtins.readFile ../../scripts/required-test.sh;
      };

      profileBootstrapApp = pkgs.writeShellApplication {
        name = "test-profile-bootstrap";
        runtimeInputs = [
          pkgs.git
          pkgs.nix
          pkgs.jq
          pkgs.openssh
          pkgs.coreutils
          pkgs.beads
        ];
        runtimeEnv.WRIX_PREK_HOOKS = wrixLib.prekHooks;
        runtimeEnv.LOOM_WRIX_BIN = "${sandbox.package}/bin/wrix";
        text = ''
          if [[ "$#" != 0 ]]; then
            printf 'this fixed fixture accepts no arguments\n' >&2
            exit 64
          fi
          exec ${pkgs.bash}/bin/bash ${../../tests/sandbox/profile-bootstrap.sh} \
            ${loom.bin}/bin/loom ${sandbox.package}/bin/wrix ${profileManifest} \
            ${pkgs.bash} ${pkgs.coreutils} ${pkgs.stdenv.hostPlatform.system}
        '';
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

      mkTransportProbe =
        setup:
        wrixLinuxPkgs.writeShellScriptBin "loom-direct-runner" ''
          set -euo pipefail
          ${setup}
          bash ${../../tests/sandbox/nix.sh}
          exec bash ${../../tests/sandbox/transport.sh}
        '';
      outageNix = wrixLinuxPkgs.writeShellScriptBin "nix" ''
        set -euo pipefail
        if [[ "''${1:-}" == copy ]]; then
          printf 'fixture upstream unavailable\n' >&2
          exit 1
        fi
        exec ${wrixLinuxPkgs.nix}/bin/nix "$@"
      '';
      transientNix = wrixLinuxPkgs.writeShellScriptBin "nix" ''
        set -euo pipefail
        if [[ "''${1:-}" == copy && ! -e /tmp/loom-transport-transient-seen ]]; then
          touch /tmp/loom-transport-transient-seen
          printf 'fixture transient transfer failure\n' >&2
          exit 1
        fi
        exec ${wrixLinuxPkgs.nix}/bin/nix "$@"
      '';
      stalledNix = wrixLinuxPkgs.writeShellScriptBin "nix" ''
        set -euo pipefail
        if [[ "''${1:-}" == copy ]]; then
          printf 'fixture transfer stalled\n' >&2
          exec ${wrixLinuxPkgs.coreutils}/bin/sleep 30
        fi
        exec ${wrixLinuxPkgs.nix}/bin/nix "$@"
      '';
      fixtureTimeout = wrixLinuxPkgs.writeShellScriptBin "timeout" ''
        set -euo pipefail
        if [[ "''${3:-}" == nix && "''${4:-}" == copy ]]; then
          exec ${wrixLinuxPkgs.coreutils}/bin/timeout --kill-after=0.1s 0.1s "''${@:3}"
        fi
        exec ${wrixLinuxPkgs.coreutils}/bin/timeout "$@"
      '';
      mkTransportSandbox =
        probe:
        wrixLib.mkSandbox {
          profile = wrixLib.profiles.base // {
            name = "verification-transport";
            mounts = [ ];
            networkAllowlist = [ "cache.nixos.org" ];
          };
          agent = "direct";
          agentPkg = probe;
        };
      transportSandbox = mkTransportSandbox (mkTransportProbe "");
      outageSandbox = mkTransportSandbox (mkTransportProbe ''export PATH="${outageNix}/bin:$PATH"'');
      transientSandbox = mkTransportSandbox (
        mkTransportProbe ''export PATH="${transientNix}/bin:$PATH"''
      );
      stalledSandbox = mkTransportSandbox (
        mkTransportProbe ''export PATH="${fixtureTimeout}/bin:${stalledNix}/bin:$PATH"''
      );
      mkTransportProfile =
        sandbox:
        pkgs.runCommand "loom-worker-transport-profile.json" { nativeBuildInputs = [ pkgs.jq ]; } ''
          set -euo pipefail
          jq '.security.runtime_secrets = {} |
              .services.beads.enable = false |
              .services.nix_cache.enable = false |
          .network.default_mode = "limit"' \
            ${sandbox.profileConfig} > "$out"
        '';
      transportProfile = mkTransportProfile transportSandbox;
      outageProfile = mkTransportProfile outageSandbox;
      transientProfile = mkTransportProfile transientSandbox;
      stalledProfile = mkTransportProfile stalledSandbox;
      transportApp = pkgs.writeShellApplication {
        name = "test-worker-transport";
        runtimeInputs = [
          pkgs.coreutils
          pkgs.nix
          pkgs.skopeo
          pkgs.jq
          pkgs.gnugrep
        ]
        ++ optionals isLinux [ pkgs.podman ];
        text = ''
          unset WRIX_UNSAFE_PODMAN_SOCKET WRIX_NETWORK WRIX_NETWORK_ALLOWLIST
          fixture=$(mktemp -d)
          trap 'rm -rf "$fixture"' EXIT
          timeout --kill-after=10s 600s ${transportSandbox.launcher}/bin/wrix \
            --profile-config ${transportProfile} run \
            --no-git-deploy --no-git-sign "$fixture"

          if ! timeout --kill-after=10s 600s ${transientSandbox.launcher}/bin/wrix \
            --profile-config ${transientProfile} run --no-git-deploy --no-git-sign "$fixture" \
            > "$fixture/transient.log" 2>&1; then
            cat "$fixture/transient.log" >&2
            exit 1
          fi
          grep -F 'fixture transient transfer failure' "$fixture/transient.log"
          grep -F 'sandbox-nix-transport-attempt-failed attempt=1 exit=1' "$fixture/transient.log"
          grep -F 'sandbox-nix-transport-preflight-ok' "$fixture/transient.log"

          if timeout --kill-after=10s 600s ${outageSandbox.launcher}/bin/wrix \
            --profile-config ${outageProfile} run --no-git-deploy --no-git-sign "$fixture" \
            > "$fixture/outage.log" 2>&1; then
            printf 'transport accepted an unavailable upstream\n' >&2
            exit 1
          else
            status=$?
          fi
          if [[ "$status" != 1 ]]; then
            cat "$fixture/outage.log" >&2
            printf 'unavailable-input fixture returned %s\n' "$status" >&2
            exit 1
          fi
          grep -F 'fixture upstream unavailable' "$fixture/outage.log"
          grep -F 'sandbox-nix-transport-attempt-failed attempt=3 exit=1' "$fixture/outage.log"

          if timeout --kill-after=10s 600s ${stalledSandbox.launcher}/bin/wrix \
            --profile-config ${stalledProfile} run --no-git-deploy --no-git-sign "$fixture" \
            > "$fixture/stalled.log" 2>&1; then
            printf 'transport accepted an interrupted transfer\n' >&2
            exit 1
          else
            status=$?
          fi
          if [[ "$status" != 124 ]]; then
            cat "$fixture/stalled.log" >&2
            printf 'interrupted-transfer fixture returned %s\n' "$status" >&2
            exit 1
          fi
          grep -F 'fixture transfer stalled' "$fixture/stalled.log"
          grep -F 'sandbox-nix-transport-attempt-failed attempt=3 exit=124' "$fixture/stalled.log"

          mkdir -m 700 "$fixture/guard"
          printf 'preserved guard fixture\n' > "$fixture/guard/sentinel"
          jq --arg guard "$fixture/guard" \
            '.profile.mounts += [{source: $guard, dest: "/homeless-shelter", mode: "ro", optional: false}]' \
            ${transportProfile} > "$fixture/guard-profile.json"
          if timeout --kill-after=10s 600s ${transportSandbox.launcher}/bin/wrix \
            --profile-config "$fixture/guard-profile.json" run --no-git-deploy --no-git-sign "$fixture" \
            > "$fixture/guard.log" 2>&1; then
            printf 'transport accepted an occupied purity guard\n' >&2
            exit 1
          else
            status=$?
          fi
          if [[ "$status" != 1 ]]; then
            cat "$fixture/guard.log" >&2
            printf 'occupied-guard fixture returned %s\n' "$status" >&2
            exit 1
          fi
          cat "$fixture/guard.log"
          grep -F 'worker-profile purity guard is occupied' "$fixture/guard.log"
          [[ "$(cat "$fixture/guard/sentinel")" == 'preserved guard fixture' ]]
          printf 'sandbox-nix-transport-failure-paths-ok\n'
        '';
      };

      nativePreflightEntry = profileManifest.manifest.rust.direct;
      missingClippy = wrixLinuxPkgs.lib.setPrio (-20) (
        wrixLinuxPkgs.writeShellScriptBin "cargo" ''
          set -euo pipefail
          if [[ "''${1:-}" == clippy ]]; then
            printf 'fixture Clippy executable unavailable\n' >&2
            exit 127
          fi
          exec ${nativeCargo}/bin/cargo "$@"
        ''
      );
      missingClippyManifest = (import ../lib.nix).mkProfileManifest {
        inherit pkgs wrixLib nativeCargo;
        imagePkgs = wrixLinuxPkgs;
        loomBin = imageLoom.bin;
        profiles.rust = rustProfile;
        runtimes.direct.agentPkg = imageLoom.bin;
        extraPackages = [ missingClippy ];
      };
      missingClippyEntry = missingClippyManifest.manifest.rust.direct;
      missingClippyProfile =
        pkgs.runCommand "loom-native-missing-clippy-profile.json"
          {
            nativeBuildInputs = [ pkgs.jq ];
          }
          ''
            jq '.security.runtime_secrets = {} |
                .services.beads.enable = false |
                .services.nix_cache.enable = false |
                .profile.mounts = [] |
                del(.profile.env.LOOM_NATIVE_PREFLIGHT)' \
              ${missingClippyEntry.profile_config} > "$out"
          '';
      missingClippyDescriptor = pkgs.writeText "loom-native-missing-clippy-manifest.json" (
        builtins.toJSON {
          rust.direct = missingClippyEntry // {
            profile_config = "${missingClippyProfile}";
          };
        }
      );
      nativePreflightDriver = loom.craneLib.mkCargoDerivation {
        pname = "loom-native-preflight-driver";
        version = "0.1.0";
        src = loom.stagedSrc;
        inherit (loom) cargoArtifacts;
        nativeBuildInputs = [ pkgs.git ];
        LOOM_TEST_NATIVE_MANIFEST = missingClippyDescriptor;
        buildPhaseCargoCommand = "cargo build --release -p loom-workflow --example native-worker-preflight";
        installPhaseCommand = ''
          install -Dm755 target/release/examples/native-worker-preflight "$out/bin/native-worker-preflight"
        '';
        doCheck = false;
      };
      nativePreflightProfile =
        pkgs.runCommand "native-preflight-fixture-profile.json" { nativeBuildInputs = [ pkgs.jq ]; }
          ''
            set -euo pipefail
            jq '.security.runtime_secrets = {} |
                .services.beads.enable = false |
                .services.nix_cache.enable = false |
                .profile.mounts = [] |
                .profile.env.LOOM_NATIVE_PREFLIGHT = "rust"' \
              ${nativePreflightEntry.profile_config} > "$out"
          '';
      nativePreflightApp = pkgs.writeShellApplication {
        name = "test-native-preflight";
        runtimeInputs = [
          pkgs.git
          pkgs.nix
          pkgs.skopeo
          pkgs.coreutils
          pkgs.gnugrep
        ]
        ++ optionals isLinux [ pkgs.podman ];
        text = ''
          unset WRIX_UNSAFE_PODMAN_SOCKET WRIX_NETWORK WRIX_NETWORK_ALLOWLIST
          unset WRIX_SIGNING_KEY WRIX_DEPLOY_KEY WRIX_PI_AUTH_FILE
          unset ANTHROPIC_API_KEY OPENAI_API_KEY CLAUDE_CODE_OAUTH_TOKEN
          fixture=$(mktemp -d)
          trap 'rm -rf "$fixture"' EXIT
          mkdir -p "$fixture/src" "$fixture/.cargo" "$fixture/scripts"
          printf '[package]\nname="native-preflight-fixture"\nversion="0.1.0"\nedition="2024"\n' > "$fixture/Cargo.toml"
          printf 'pub fn fixture() {}\n' > "$fixture/src/lib.rs"
          cp ${../../Cargo.lock} "$fixture/Cargo.lock"
          cp ${../../.cargo/config.toml} "$fixture/.cargo/config.toml"
          cp ${../../scripts/cargo-patch-key.nix} "$fixture/scripts/cargo-patch-key.nix"
          cp ${../../scripts/configure-cargo-patches.sh} "$fixture/scripts/configure-cargo-patches.sh"
          git -C "$fixture" init -q
          if ! timeout --kill-after=10s 400s ${nativePreflightEntry.launcher} \
            --profile-config ${nativePreflightProfile} run --no-git-deploy --no-git-sign "$fixture" -- --help \
            > "$fixture/preflight.log" 2>&1; then
            cat "$fixture/preflight.log" >&2
            exit 1
          fi
          cat "$fixture/preflight.log"
          grep -F 'source-feedback tools available; required verification remains pending' "$fixture/preflight.log"
          [[ -s "$fixture/.cargo/nix-patches.toml" ]]
          mkdir "$fixture/unavailable"
          git -C "$fixture/unavailable" init -q
          printf 'broken manifest awaiting source repair\n' > "$fixture/unavailable/Cargo.toml"
          if ! timeout --kill-after=10s 400s ${nativePreflightEntry.launcher} \
            --profile-config ${nativePreflightProfile} run --no-git-deploy --no-git-sign "$fixture/unavailable" -- --help \
            > "$fixture/unavailable.log" 2>&1; then
            cat "$fixture/unavailable.log" >&2
            printf 'native preflight blocked the agent from repairing its source\n' >&2
            exit 1
          fi
          grep -F 'project metadata unavailable; source repair remains enabled' "$fixture/unavailable.log"
          grep -F 'source-feedback tools available; required verification remains pending' "$fixture/unavailable.log"
          mkdir -m 700 "$fixture/driver-home"
          HOME="$fixture/driver-home" \
            XDG_STATE_HOME="$fixture/driver-home/.local/state" \
            XDG_CONFIG_HOME="$fixture/driver-home/.config" \
            XDG_CACHE_HOME="$fixture/driver-home/.cache" \
            GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
            timeout --kill-after=10s 600s ${nativePreflightDriver}/bin/native-worker-preflight
          printf 'native-preflight-actual-worker-ok\n'
        '';
      };
    in
    {
      packages.smoke-runtime = smokeRuntime;

      apps = {
        test-profile-bootstrap = {
          type = "app";
          program = "${profileBootstrapApp}/bin/test-profile-bootstrap";
        };
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
          meta.description = "Offline agent health, private Nix builds, and hook-chain checks for the Rust sandbox image (Linux only; Darwin stub)";
        };

        test-worker-transport = {
          type = "app";
          program = "${transportApp}/bin/test-worker-transport";
          meta.description = "Signed input transport in a provisioned isolated verifier";
        };

        test-native-preflight = {
          type = "app";
          program = "${nativePreflightApp}/bin/test-native-preflight";
          meta.description = "Native availability and pinned patch initialization in an isolated worker";
        };

        fuzz-loom = {
          type = "app";
          program = "${fuzzApp}/bin/fuzz-loom";
          meta.description = "On-demand cargo fuzz runner (not gated by flake check)";
        };
      };
    };
}

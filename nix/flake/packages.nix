_:

{
  perSystem =
    {
      pkgs,
      config,
      sandbox,
      debugSandbox,
      imagePiCodingAgent,
      loom,
      loomBin,
      wrixSrc,
      wrixLauncher,
      profileManifest,
      ...
    }:
    let
      nativePackage = import ../native-package.nix;
      launchApp =
        {
          name,
          profileConfig,
          launcher ? wrixLauncher,
        }:
        pkgs.writeShellApplication {
          inherit name;
          text = ''
            exec ${launcher}/bin/wrix --profile-config ${pkgs.lib.escapeShellArg profileConfig} run "$@"
          '';
        };
      fixtureLauncher = pkgs.writeShellScriptBin "wrix" ''
        set -euo pipefail
        printf '%s\n' "$@"
      '';
      fixtureApp = launchApp {
        name = "sandbox-launch-fixture";
        profileConfig = "/selected profile.json";
        launcher = fixtureLauncher;
      };
    in
    {
      packages = {
        profileManifest = nativePackage profileManifest;
        profile-images = nativePackage profileManifest;

        cargo-patches = nativePackage loom.cargoConfig;
        default = loom.bin;
        loom = loom.bin;
        verification-build = loom.verificationBuild;
        verification-walks = config.checks.loom-gate-check;
        verification-source-integrity = config.checks.native-stage-integrity;
        verification-llm-compat = config.checks.llm-compat-report;
        verification-llm-compat-compile = config.checks.llm-compat-compile;
        verification-llm-native-compile = config.checks.llm-native-compile;
        verification-clippy = loom.clippy;
        verification-clippy-all-targets = loom.clippyAllTargets;
        verification-nextest = loom.nextest;
        loom-wrix = nativePackage loomBin;
        pi-coding-agent = nativePackage imagePiCodingAgent;

        debug = nativePackage (launchApp {
          name = "loom-debug";
          profileConfig = "${debugSandbox.image.profileConfig}";
        });
        sandbox = nativePackage (launchApp {
          name = "loom-sandbox";
          profileConfig = "${sandbox.image.profileConfig}";
        });
        sandbox-image = nativePackage sandbox.image;

        wrixSrc = nativePackage (
          pkgs.runCommand "wrix-src" { } ''
            cp -r ${wrixSrc} $out
          ''
        );
      };

      checks.sandbox-launch-command = nativePackage (
        pkgs.runCommand "sandbox-launch-command" { } ''
          set -euo pipefail
          ${fixtureApp}/bin/sandbox-launch-fixture '/workspace with spaces' -- --version > actual
          printf '%s\n' --profile-config '/selected profile.json' run '/workspace with spaces' -- --version > expected
          cmp actual expected
          touch "$out"
        ''
      );
    };
}

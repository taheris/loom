_:

{
  perSystem =
    {
      pkgs,
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
        inherit profileManifest;
        profile-images = profileManifest;

        cargo-patches = loom.cargoConfig;
        default = loom.bin;
        loom = loom.bin;
        loom-wrix = loomBin;
        pi-coding-agent = imagePiCodingAgent;

        debug = launchApp {
          name = "loom-debug";
          profileConfig = "${debugSandbox.image.profileConfig}";
        };
        sandbox = launchApp {
          name = "loom-sandbox";
          profileConfig = "${sandbox.image.profileConfig}";
        };
        sandbox-image = sandbox.image;

        wrixSrc = pkgs.runCommand "wrix-src" { } ''
          cp -r ${wrixSrc} $out
        '';
      };

      checks.sandbox-launch-command = pkgs.runCommand "sandbox-launch-command" { } ''
        set -euo pipefail
        ${fixtureApp}/bin/sandbox-launch-fixture '/workspace with spaces' -- --version > actual
        printf '%s\n' --profile-config '/selected profile.json' run '/workspace with spaces' -- --version > expected
        cmp actual expected
        touch "$out"
      '';
    };
}

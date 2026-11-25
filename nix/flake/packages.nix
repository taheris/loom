_:

{
  perSystem =
    {
      pkgs,
      config,
      sandbox,
      debugSandbox,
      wrixModules,
      loom,
      loomBin,
      wrixSrc,
      wrixLauncher,
      profileManifest,
      profileCatalogue,
      ...
    }:
    let
      nativePackage = import ../native-package.nix;
      ownedProfileManifest = nativePackage (
        import ../profile-manifest-artifact.nix {
          inherit pkgs;
          manifest = profileManifest;
        }
      );
      checkedPreparation =
        build:
        (import ../verification.nix).artifact {
          runtime = "native-process";
          resources = [ ];
          # Cargo source selection determines SDK identity, not shipped runtime code.
          evaluation = (import ../native-context.nix) ++ [ loom.workspaceSource ];
          inherit build;
        };
      verificationSdk = pkgs.closureInfo {
        rootPaths = [
          loom.preparationInputs
          (import ../quint/tools.nix { inherit pkgs; })
          pkgs.shellcheck-minimal
          pkgs.cargo-fuzz
          pkgs.makeWrapper
          config.treefmt.build.wrapper
          ../../flake.lock
          ../../Cargo.lock
        ];
      };
      verificationPreparation = (import ../verification.nix).artifact {
        runtime = "native-process";
        resources = [ ];
        # The SDK identity is computed from Cargo sources during evaluation;
        # building this launcher must not acquire or stage the SDK itself.
        evaluation = (import ../native-context.nix) ++ [ loom.workspaceSource ];
        build =
          _:
          import ../verification-preparation.nix {
            inherit pkgs;
            sdk = verificationSdk;
          };
      };
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
      # Keep `nix build .#profile-catalogue` as the rooted data interface. This
      # value has no executable recipe, so it is not a packages derivation.
      legacyPackages.profile-catalogue = profileCatalogue.outPath;
      legacyPackages.loomFeedback = loom.scopedNextest;
      packages = {
        profileManifest = ownedProfileManifest;
        profile-images = ownedProfileManifest;

        verification-sdk = checkedPreparation (_: verificationSdk);
        prepare-verification = verificationPreparation;
        cargo-patches = nativePackage loom.cargoConfig;
        default = loom.bin;
        loom = loom.bin;
        loom-launcher = loom.runtimeBin;
        verification-tool = import ../verification-tool.nix { inherit pkgs; };
        verification-build = loom.verificationBuild;
        verification-agent-landing = config.checks.agent-landing;
        verification-kernel-free-surface = config.checks.kernel-free-surface;
        verification-confined-runtime = config.checks.confined-runtime;
        verification-walks = config.checks.loom-gate-check;
        verification-source-integrity = config.checks.native-stage-integrity;
        verification-test-app-hook-runner = config.checks.test-app-hook-runner;
        verification-acceptance-storage-report = config.checks.acceptance-storage-report;
        verification-native-smoke-beads = config.checks.native-smoke-beads;
        verification-llm-compat = config.checks.llm-compat-report;
        verification-llm-compat-compile = config.checks.llm-compat-compile;
        verification-llm-native-compile = config.checks.llm-native-compile;
        verification-clippy = loom.clippy;
        verification-clippy-all-targets = loom.clippyAllTargets;
        verification-nextest = loom.nextest;
        loom-wrix = nativePackage loomBin;
        pi-coding-agent = nativePackage (wrixModules.pi { inherit pkgs; });

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

      apps.prepare-verification = (import ../native-app.nix) pkgs {
        package = verificationPreparation;
        name = "prepare-verification";
        meta.description = "Prepare retained verification inputs, or check readiness offline.";
      };

      checks.sandbox-launch-command = (import ../verification.nix).check {
        runtime = "native-process";
        resources = [ ];
        evaluation = import ../native-context.nix;
        build =
          _:
          pkgs.runCommand "sandbox-launch-command" { } ''
            set -euo pipefail
            ${fixtureApp}/bin/sandbox-launch-fixture '/workspace with spaces' -- --version > actual
            printf '%s\n' --profile-config '/selected profile.json' run '/workspace with spaces' -- --version > expected
            cmp actual expected
            touch "$out"
          '';
      };
    };
}

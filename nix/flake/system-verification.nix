_: {
  perSystem =
    {
      config,
      pkgs,
      system,
      loom,
      profileBootstrapRegistration,
      ...
    }:
    let
      verify = import ../system-verification.nix { inherit pkgs system loom; };
      ciShell = pkgs.writeShellApplication {
        name = "ci-shell-scenario";
        runtimeInputs = [ pkgs.nix ];
        text = "exec nix --extra-experimental-features 'nix-command flakes' develop --offline .#ci -c loom --version";
      };
      ciRuntimeRegistration = pkgs.closureInfo {
        rootPaths = [
          config.devShells.ci
          pkgs.bashInteractive
        ];
      };
      ciRegistration = pkgs.runCommand "ci-shell-registration" { } ''
        cat ${loom.testRegistration}/registration ${ciRuntimeRegistration}/registration > "$out"
      '';
      scenarios = {
        verification-system-smoke = {
          name = "smoke";
          nixRegistration = "${loom.testRegistration}/registration";
          program = config.apps.smoke.program;
        };
        verification-system-profile-bootstrap = {
          name = "profile-bootstrap";
          nixRegistration = "${profileBootstrapRegistration}/registration";
          program = config.apps.test-profile-bootstrap.program;
        };
        verification-system-ci-shell = {
          name = "ci-shell";
          nixRegistration = ciRegistration;
          program = "${ciShell}/bin/ci-shell-scenario";
        };
        verification-system-decision-admission = {
          name = "decision-admission";
          nixRegistration = "${loom.testRegistration}/registration";
          program = config.apps.test-decision-admission.program;
        };
      };
      # Darwin's read-only sandbox cannot host a writable canonical Nix store.
      packages = builtins.mapAttrs (
        _: scenario: if pkgs.stdenv.hostPlatform.isDarwin then loom.nextest else verify scenario
      ) scenarios;
      programs = builtins.toFile "native-system-programs.json" (
        builtins.toJSON (
          builtins.listToAttrs (
            map (scenario: {
              inherit (scenario) name;
              value =
                let
                  recipes = builtins.attrNames (builtins.getContext scenario.program);
                  recipe = builtins.head recipes;
                in
                assert builtins.length recipes == 1;
                {
                  program = builtins.unsafeDiscardStringContext scenario.program;
                  derivation = builtins.appendContext recipe { "${recipe}".path = true; };
                };
            }) (builtins.attrValues scenarios)
          )
        )
      );
    in
    {
      inherit packages;
      legacyPackages.native-system-programs = programs;
    };
}

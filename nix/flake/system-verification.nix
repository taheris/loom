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
      bootstrapRegistration = pkgs.runCommand "profile-bootstrap-registration" { } ''
        cat ${loom.testRegistration}/registration ${profileBootstrapRegistration}/registration > "$out"
      '';
      scenarios = {
        verification-system-smoke = {
          name = "smoke";
          runtime = "native-container";
          nixRegistration = "${loom.testRegistration}/registration";
          program = "${config.packages.smoke-runtime}/bin/smoke";
        };
        verification-system-profile-bootstrap = {
          name = "profile-bootstrap";
          nixRegistration = bootstrapRegistration;
          program = config.apps.test-profile-bootstrap.program;
        };
        verification-system-ci-shell = {
          name = "ci-shell";
          nixRegistration = "${loom.testRegistration}/registration";
          program = "${ciShell}/bin/ci-shell-scenario";
        };
        verification-system-decision-admission = {
          name = "decision-admission";
          nixRegistration = "${loom.testRegistration}/registration";
          program = config.apps.test-decision-admission.program;
        };
      };
    in
    {
      packages = builtins.mapAttrs (_: scenario: verify scenario) scenarios;
    };
}

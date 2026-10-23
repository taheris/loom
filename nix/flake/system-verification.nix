_: {
  perSystem =
    {
      config,
      pkgs,
      system,
      loom,
      ...
    }:
    let
      verify = import ../system-verification.nix { inherit pkgs system loom; };
      ciShell = pkgs.writeShellApplication {
        name = "ci-shell-scenario";
        runtimeInputs = [ pkgs.nix ];
        text = "exec nix develop --offline .#ci -c loom --version";
      };
      scenarios = {
        verification-system-smoke = {
          name = "smoke";
          program = config.apps.smoke.program;
        };
        verification-system-profile-bootstrap = {
          name = "profile-bootstrap";
          program = config.apps.test-profile-bootstrap.program;
        };
        verification-system-ci-shell = {
          name = "ci-shell";
          program = "${ciShell}/bin/ci-shell-scenario";
        };
        verification-system-decision-admission = {
          name = "decision-admission";
          program = config.apps.test-decision-admission.program;
        };
      };
    in
    {
      packages = builtins.mapAttrs (_: scenario: verify scenario) scenarios;
    };
}

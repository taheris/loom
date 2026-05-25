_:

{
  perSystem =
    {
      pkgs,
      sandbox,
      debugSandbox,
      loomBin,
      profileManifest,
      ...
    }:
    let
      inherit (pkgs) lib;
      smokeApp = pkgs.writeShellApplication {
        name = "test";
        runtimeInputs = [
          loomBin
          pkgs.podman
          pkgs.jq
          pkgs.git
        ];
        text = builtins.readFile ../../tests/run-tests.sh;
      };
      darwinStub = pkgs.writeShellApplication {
        name = "test";
        text = ''
          echo "container smoke not available on Darwin"
          exit 0
        '';
      };
      testApp = if lib.hasSuffix "linux" pkgs.stdenv.hostPlatform.system then smokeApp else darwinStub;
    in
    {
      packages = {
        default = sandbox.package;
        sandbox = sandbox.package;
        debug = debugSandbox.package;
        loom = loomBin;
        test = testApp;
        inherit profileManifest;
      };

      apps.test = {
        type = "app";
        program = "${testApp}/bin/test";
      };
    };
}

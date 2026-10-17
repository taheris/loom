{ pkgs, profileManifest }:
let
  loomLib = import ../../nix/lib.nix;
  agent = pkgs.writeShellScriptBin "pi" ''
    printf 'factory-pi-started\n'
  '';
  imageFile = pkgs.writeText "factory-image" "image";
  image = imageFile // {
    profileConfig = imageFile;
  };
  # Wrix is the external boundary. Reject any new policy or UID arguments:
  # provisioning and execution belong to the ordinary constructor alone.
  wrixLib = {
    capabilities.privateNix = 1;
    mkSandbox =
      {
        profile,
        agent,
        packages,
        agentPkg,
      }:
      {
        inherit
          profile
          agent
          packages
          agentPkg
          image
          ;
        launcher = agentPkg;
      };
    mkProfileImages = images: {
      passthru.manifest = builtins.mapAttrs (runtime: _: {
        ${runtime} = {
          ref = "test";
        };
      }) images;
    };
  };
  arguments = {
    inherit pkgs wrixLib;
    loomBin = agent;
    profiles = {
      base.packages = [ ];
      rust.packages = [ ];
      python.packages = [ ];
    };
    runtimes = builtins.mapAttrs (_: _: { agentPkg = agent; }) {
      pi = null;
      claude = null;
      direct = null;
    };
  };
  factory = loomLib.mkProfileManifest arguments;
  withLibrary = lib: loomLib.mkProfileManifest (arguments // { wrixLib = lib; });
  legacy = withLibrary (builtins.removeAttrs wrixLib [ "capabilities" ]);
  unsupported = withLibrary (wrixLib // { capabilities.privateNix = 2; });
  rejected = output: !(builtins.tryEval (builtins.deepSeq output.manifest true)).success;
  allRuntimes = predicate: builtins.all predicate (builtins.attrNames arguments.runtimes);
in
assert builtins.all (
  profile:
  builtins.all (
    runtime: profileManifest.sandboxes.${profile}.${runtime}.image.labels."wrix.nix.private" == "1"
  ) profileManifest.runtimes
) profileManifest.profiles;
assert rejected legacy;
assert rejected unsupported;
assert legacy.sandboxes.base.pi.agent == "pi";
assert allRuntimes (runtime: factory.sandboxes.rust.${runtime}.agent == runtime);
assert allRuntimes (runtime: factory.manifest.rust.${runtime}.native_preflight == "rust");
assert allRuntimes (runtime: !(factory.manifest.base.${runtime} ? native_preflight));
assert allRuntimes (runtime: !(factory.manifest.python.${runtime} ? native_preflight));
pkgs.runCommand "native-factory-private-nix" { } ''
  set -euo pipefail
  unset LOOM_NATIVE_PREFLIGHT
  [[ "$(${factory.sandboxes.rust.pi.agentPkg}/bin/pi --version)" == factory-pi-started ]]
  touch "$out"
''

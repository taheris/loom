{
  pkgs,
  crane,
  src,
  verificationTool ? import ../../nix/verification-tool.nix { inherit pkgs; },
}:
let
  inherit (pkgs.lib) mapAttrs;
  api = import ../../nix/lib.nix;
  nativePackage = import ../../nix/native-package.nix;
  artifact = pkgs.writeText "consumer-native-artifact" "artifact, not isolated execution";
  expression = pkgs.writeText "consumer-cold-evaluation.nix" ''
    let
      pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; };
      workspace = (import ${src}/nix/lib.nix).mkLoom {
        inherit pkgs;
        crane.mkLib = pkgs: import ${crane}/default.nix { inherit pkgs; };
        fenix = {};
        src = ${src};
        toolchain = pkgs.symlinkJoin {
          name = "consumer-evaluation-toolchain";
          paths = [ pkgs.cargo pkgs.rustc ];
        };
        cargoToml = ${src}/Cargo.toml;
        cargoLock = ${src}/Cargo.lock;
      };
    in {
      fixed = {
        check = (workspace.mkCheck { cargoNextestExtraArgs = "-p loom-driver"; }).drvPath;
        metadata = toString workspace.cargoToml;
        lock = toString workspace.cargoLock;
        vendor = toString workspace.cargoVendorDir;
        staged = toString workspace.stagedSrc;
      };
      legacy = (workspace.craneLib.cargoNextest {
        src = workspace.stagedSrc;
        inherit (workspace) cargoArtifacts;
        cargoNextestExtraArgs = "-p loom-driver";
      }).drvPath;
    }
  '';
  fakeWrix = {
    capabilities.privateNix = 1;
    rustPackage.wrix = throw "image manifests must not select host executables";
    profiles = {
      base = {
        name = "base";
        packages = [ ];
      };
      rust = {
        name = "rust";
        packages = [ ];
      };
    };
    mkSandbox = { agent, profile, ... }: {
      image = {
        source = pkgs.writeText "consumer-${agent}-image" "image provenance fixture";
        profileConfig = pkgs.writeText "consumer-${profile.name}-${agent}-config.json" ''{"agent":{"kind":"${agent}"},"image":{"digest":"fixture-digest"}}'';
      };
    };
    mkProfileImages = images: {
      passthru.manifest = mapAttrs (runtime: image: {
        ${runtime} = {
          ref = "fixture-${runtime}";
          source = toString image.source;
          source_kind = "nix-descriptor";
        };
      }) images;
    };
  };
  manifest = api.mkProfileManifest {
    inherit pkgs;
    wrixLib = fakeWrix;
    profiles = fakeWrix.profiles;
    loomBin = pkgs.writeText "consumer-loom-fixture" "";
    piAgentPkg = pkgs.writeText "consumer-agent-fixture" "";
    runtimes.pi = { };
  };
in
assert (nativePackage artifact).loom.runtime == "native-process";
assert builtins.all
  (runtime: (nativePackage (artifact // { loom = { inherit runtime; }; })).loom.runtime == runtime)
  [
    "nix"
    "native-cgroup"
    "native-container"
  ];
pkgs.runCommand "loom-consumer-api"
  {
    nativeBuildInputs = [
      (pkgs.lib.getBin pkgs.nix)
      pkgs.coreutils
      verificationTool
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR/home"
    mkdir -p "$HOME"
    loom-verification consumer-api ${expression} ${manifest} ${manifest.nativePreflight}
    touch "$out"
  ''

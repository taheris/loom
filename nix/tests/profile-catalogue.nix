{ pkgs, wrixLib }:
let
  inherit (builtins) attrValues getContext toJSON;
  loomLib = import ../lib.nix;
  make =
    generation:
    let
      external = wrixLib // {
        mkSandbox =
          {
            profile,
            agent,
            agentPkg ? null,
            ...
          }:
          {
            image =
              let
                selected = profile.name == "rust" && agent == "direct";
                source =
                  if selected then
                    pkgs.writeText "selected-image-${generation}" "image payload"
                  else
                    pkgs.runCommand "unselected-${profile.name}-${agent}" { } ''
                      echo 'unselected image demanded' >&2
                      exit 1
                    '';
                profileConfig = pkgs.writeText "selected-config.json" (toJSON {
                  inherit agent;
                  source = "${source}";
                  agent_package = if agentPkg == null then null else "${agentPkg}";
                });
              in
              {
                inherit agent source profileConfig;
                ref = "${profile.name}-${agent}";
                source_kind = "nix-descriptor";
              };
          };
      };
    in
    loomLib.mkProfileCatalogue {
      inherit pkgs;
      imagePkgs = pkgs;
      wrixLib = external;
      loomBin = pkgs.writeShellScriptBin "loom-direct-runner" "exit 0";
      piAgentPkg = pkgs.writeShellScriptBin "pi" "exit 0";
      claudeAgentPkg = pkgs.writeShellScriptBin "claude" "exit 0";
      profiles = {
        base.name = "base";
        rust.name = "rust";
        python.name = "python";
      };
    };
  original = make "original";
  changed = make "changed";
  text = toJSON original.catalogue;
in
assert builtins.all (context: context == { path = true; }) (attrValues (getContext text));
assert original.provisions.rust.direct.drvPath != changed.provisions.rust.direct.drvPath;
assert original.provisions.base.pi.drvPath == changed.provisions.base.pi.drvPath;
assert original.provisions.python.claude.drvPath == changed.provisions.python.claude.drvPath;
pkgs.runCommand "loom-profile-catalogue-contract"
  {
    nativeBuildInputs = [ pkgs.jq ];
    passthru.catalogue = original;
  }
  ''
    jq -e '.schema == 1 and (.profiles | length) == 3 and all(.profiles[]; length == 3)' ${original}
    jq -e '.rust.direct.native_preflight == "rust" and .rust.direct.ref == "rust-direct" and (keys == ["rust"]) and (.rust | keys == ["direct"])' ${original.provisions.rust.direct}
    touch "$out"
  ''

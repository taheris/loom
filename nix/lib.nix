# Pure Nix functions exposed via `loom.lib.*`. No flake-parts knowledge — also
# callable from non-flake contexts via `import ./nix/lib.nix`.

let
  inherit (builtins) attrNames mapAttrs;
  workspaceBuilder = import ./workspace.nix;
in
{
  # Build the loom Cargo workspace. Returns `{ bin, clippy, nextest,
  # cargoArtifacts, craneLib, toolchain }`. See `nix/workspace.nix`.
  mkLoom = workspaceBuilder;

  # Build a `profile-images.json` derivation for the given profile/runtime
  # matrix, suitable for `export LOOM_PROFILES_MANIFEST=...`. `profiles` is an
  # attrset of wrix profile definitions; default covers the three wrix ships out
  # of the box. The manifest is keyed first by profile, then by agent runtime.
  # `loomBin` is threaded into every sandbox so the in-container PATH always
  # carries `loom`; the direct runtime also uses it as the default
  # `loom-direct-runner` provider.
  mkProfileManifest =
    {
      pkgs,
      wrixLib,
      loomBin,
      profiles ? { inherit (wrixLib.profiles) base rust python; },
      # Back-compat for callers that previously selected the single runtime via
      # `agent` and optionally overrode its package via `agentPkg`. The manifest
      # is now always a runtime matrix; when `agent` is set, `agentPkg` applies
      # to that runtime. With no `agent`, legacy `agentPkg` still means Pi.
      agent ? null,
      agentPkg ? null,
      piAgentPkg ? if agent == null || agent == "pi" then agentPkg else null,
      claudeAgentPkg ? if agent == "claude" then agentPkg else null,
      directAgentPkg ? if agent == "direct" && agentPkg != null then agentPkg else loomBin,
      runtimes ? {
        claude = {
          agentPkg = claudeAgentPkg;
        };
        direct = {
          agentPkg = directAgentPkg;
        };
        pi = {
          agentPkg = piAgentPkg;
        };
      },
    }:
    let
      inherit (pkgs.lib) optionalAttrs;

      mkSandboxForRuntime =
        profile: runtime: runtimeConfig:
        let
          agentPkgOverride = runtimeConfig.agentPkg or null;
        in
        wrixLib.mkSandbox (
          {
            inherit profile;
            agent = runtime;
            packages = [ loomBin ];
          }
          // optionalAttrs (agentPkgOverride != null) {
            agentPkg = agentPkgOverride;
          }
        );

      sandboxes = mapAttrs (
        _profileName: profile:
        mapAttrs (runtime: runtimeConfig: mkSandboxForRuntime profile runtime runtimeConfig) runtimes
      ) profiles;
      images = mapAttrs (
        _profileName: byRuntime: mapAttrs (_runtime: sandbox: sandbox.image) byRuntime
      ) sandboxes;
      profileManifests = mapAttrs (_profileName: byRuntime: wrixLib.mkProfileImages byRuntime) images;
      manifest = mapAttrs (
        profileName: profileManifest:
        mapAttrs (
          runtime: entry:
          entry
          // optionalAttrs (images.${profileName}.${runtime} ? digest) {
            digest = "${images.${profileName}.${runtime}.digest}";
          }
        ) profileManifest.passthru.manifest
      ) profileManifests;
    in
    pkgs.writeTextFile {
      name = "profile-images.json";
      text = builtins.toJSON manifest;
      passthru = {
        inherit manifest;
        profiles = attrNames profiles;
        runtimes = attrNames runtimes;
      };
    };

  # Wrap the raw loom binary with the matching sandbox launcher defaults, so a
  # consumer only needs the wrapped binary on PATH to run loom end-to-end.
  # Consumers can still override either env var.
  mkLoomBin =
    {
      pkgs,
      loomBuild,
      wrixLauncher,
      profileManifest,
    }:
    pkgs.runCommand "loom"
      {
        nativeBuildInputs = [ pkgs.makeWrapper ];
        inherit (loomBuild.bin) meta;
      }
      ''
        mkdir -p $out/bin
        makeWrapper ${loomBuild.bin}/bin/loom $out/bin/loom \
          --prefix PATH : ${wrixLauncher}/bin \
          --set-default LOOM_WRIX_BIN ${wrixLauncher}/bin/wrix \
          --set-default LOOM_PROFILES_MANIFEST ${profileManifest}
      '';
}

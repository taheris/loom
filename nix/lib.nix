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
      extraPackages ? [ ],
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
            packages = [ loomBin ] ++ extraPackages;
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
          let
            image = images.${profileName}.${runtime};
          in
          entry.${runtime}
          // {
            inherit runtime;
            # wrix's manifest helper serializes path strings for portability,
            # but strips their Nix string context. Re-attach the concrete
            # image source here so LOOM_PROFILES_MANIFEST keeps runtime inputs
            # alive in the store after GC / on fresh machines.
            source = "${image.source or image}";
          }
          // optionalAttrs (sandboxes.${profileName}.${runtime} ? launcher) {
            # The raw launcher accepts Loom's per-bead ProfileConfig. Do not
            # use `sandbox.package` here: that configured wrapper already
            # injects its own `--profile-config`, which would collide with the
            # runtime-selected one below.
            launcher = "${sandboxes.${profileName}.${runtime}.launcher}/bin/wrix";
          }
          // optionalAttrs (image ? profileConfig) {
            # `wrix spawn` now requires the immutable ProfileConfig path; keep
            # it as a real Nix reference, not just inert JSON text.
            profile_config = "${image.profileConfig}";
          }
          // optionalAttrs (image ? digest) {
            digest = "${image.digest}";
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
  # Consumers can still override the env defaults.
  mkLoomBin =
    {
      pkgs,
      loomBuild,
      wrixLauncher,
      profileManifest,
    }:
    let
      inherit (pkgs) stdenv;
      inherit (pkgs.lib) makeBinPath optionals;

      spawnLauncher = wrixLauncher.launcher or wrixLauncher;
      launcherRuntimePath = makeBinPath (
        [ pkgs.nix ]
        ++ optionals stdenv.hostPlatform.isLinux [
          pkgs.podman
          pkgs.skopeo
        ]
        ++ optionals stdenv.hostPlatform.isDarwin [ pkgs.skopeo ]
      );
    in
    pkgs.runCommand "loom"
      {
        nativeBuildInputs = [ pkgs.makeWrapper ];
        inherit (loomBuild.bin) meta;
      }
      ''
        mkdir -p $out/bin
        makeWrapper ${loomBuild.bin}/bin/loom $out/bin/loom \
          --prefix PATH : ${wrixLauncher}/bin:${spawnLauncher}/bin:${launcherRuntimePath} \
          --set-default LOOM_WRIX_BIN ${wrixLauncher}/bin/wrix \
          --set-default LOOM_WRIX_SPAWN_BIN ${spawnLauncher}/bin/wrix \
          --set-default LOOM_PROFILES_MANIFEST ${profileManifest}
      '';
}

# pi-mono coding agent packaged for the loom sandbox.
#
# Mirrors the `lib/pi-mono/` pattern wrix used to ship: the published
# @mariozechner/pi-coding-agent npm tarball already contains a pre-built
# `dist/` (the upstream `prepublishOnly` script runs `tsgo` before
# publish), so this derivation only resolves and stages dependencies —
# no JS build runs in Nix.
#
# `package.json` is the single source of truth for the pinned version
# — no separate constant elsewhere needs to be kept in sync.
#
# Bumping the pin:
#   1. Update "@mariozechner/pi-coding-agent" version in ./package.json.
#   2. Update the `version` attribute below to match.
#   3. Regenerate the lockfile and audit install scripts:
#        cd nix/pi-mono
#        rm package-lock.json
#        npm install --omit=dev --ignore-scripts --package-lock-only
#        node -e 'for (const [p,i] of Object.entries(require("./package-lock.json").packages))
#          if (i.hasInstallScript) console.log(p);'
#      For each new entry, inspect the script source — refuse the bump
#      if any reaches the network or does anything other than benign
#      metadata processing. The 0.73.1 audit cleared protobufjs (benign
#      version-scheme warning), koffi (prebuild selection; native .node
#      bundles already shipped), and @google/genai (no-op on registry
#      installs).
#   4. Set `npmDepsHash` below to `lib.fakeHash`, run
#      `nix build .#pi-mono`, copy the suggested hash back in.
{
  lib,
  buildNpmPackage,
  nodejs_22,
  makeWrapper,
}:

let
  inherit (lib) fileset licenses platforms;
in

buildNpmPackage (_finalAttrs: {
  pname = "pi-mono";
  version = "0.73.1";

  nodejs = nodejs_22;

  src = fileset.toSource {
    root = ./.;
    fileset = fileset.unions [
      ./package.json
      ./package-lock.json
    ];
  };

  npmDepsHash = "sha256-xVg3/K1ulEjft6DvLyXGXyz1vAtqyAEpS1t0vJFDwGw=";

  # Upstream tarball ships pre-built dist/cli.js — no JS build step.
  dontNpmBuild = true;

  # Skip npm lifecycle scripts. Audited for 0.73.1 (see header comment).
  npmFlags = [ "--ignore-scripts" ];

  nativeBuildInputs = [ makeWrapper ];

  postInstall = ''
    mkdir -p "$out/bin"
    makeWrapper ${nodejs_22}/bin/node "$out/bin/pi" \
      --add-flags "$out/lib/node_modules/loom-pi-mono-launcher/node_modules/@mariozechner/pi-coding-agent/dist/cli.js"
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    "$out/bin/pi" --version >/dev/null
    runHook postInstallCheck
  '';

  meta = {
    description = "pi-mono coding agent (RPC runtime for the loom pi backend)";
    homepage = "https://github.com/badlogic/pi-mono";
    license = licenses.mit;
    mainProgram = "pi";
    platforms = platforms.linux ++ platforms.darwin;
  };
})

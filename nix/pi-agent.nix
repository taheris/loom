# Pi 1.5.2 already canonicalizes auth paths. Retain Wrix's matching 30s
# reader/refresh lease without applying its obsolete pre-1.5.2 source hunk.
{ pkgs, authStorageTest }:
pkgs.pi-coding-agent.overrideAttrs (old: {
  postPatch = (old.postPatch or "") + ''
    substituteInPlace packages/coding-agent/src/core/auth-storage.ts \
      --replace-fail 'return lockfile.lockSync(path);' \
        'return lockfile.lockSync(path, { realpath: true, stale: 30_000 });'
  '';
  postInstallCheck = (old.postInstallCheck or "") + ''
    PI_AUTH_TEST_PACKAGE="$out/lib/node_modules/pi-monorepo" \
      ${pkgs.nodejs}/bin/node ${authStorageTest}
  '';
})

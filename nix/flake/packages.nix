{ inputs, ... }:

{
  perSystem =
    {
      pkgs,
      sandbox,
      debugSandbox,
      loom,
      loomBin,
      profileManifest,
      ...
    }:
    {
      packages = {
        inherit profileManifest;
        profile-images = profileManifest;

        default = loom.bin;
        loom = loom.bin;
        loom-wrix = loomBin;

        debug = debugSandbox.package;
        sandbox = sandbox.package;
        sandbox-image = sandbox.image;

        # Pinned wrix source, exposed so spec verifiers in this repo can
        # grep against the same `lib/sandbox/linux/entrypoint.sh` the
        # sandbox image actually runs. The wrix source lives in a
        # separate flake input, so the verifier resolves the path through
        # `nix build --no-link --print-out-paths .#wrixSrc` (see
        # `specs/agent.md` § Container integration).
        wrixSrc = pkgs.runCommand "wrix-src" { } ''
          cp -r ${inputs.wrix} $out
        '';
      };
    };
}

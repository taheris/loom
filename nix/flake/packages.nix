{ inputs, ... }:

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
    {
      packages = {
        default = sandbox.package;
        sandbox = sandbox.package;
        debug = debugSandbox.package;
        loom = loomBin;
        inherit profileManifest;
        # Pinned wrapix source, exposed so spec verifiers in this repo can
        # grep against the same `lib/sandbox/linux/entrypoint.sh` the
        # sandbox image actually runs. The wrapix source lives in a
        # separate flake input, so the verifier resolves the path through
        # `nix build --no-link --print-out-paths .#wrapixSrc` (see
        # `specs/agent.md` § Container integration).
        wrapixSrc = pkgs.runCommand "wrapix-src" { } ''
          cp -r ${inputs.wrapix} $out
        '';
      };
    };
}

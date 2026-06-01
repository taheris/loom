_:

{
  perSystem =
    { pkgs, loom, ... }:
    let
      inherit (loom)
        bin
        cargoArtifacts
        craneLib
        stagedSrc
        ;

      loom-gate-check = craneLib.mkCargoDerivation {
        pname = "loom-gate-check";
        version = "0.0.0";
        src = stagedSrc;
        inherit cargoArtifacts;

        doCheck = true;
        # Terminal derivation — nothing downstream consumes our `target/`,
        # so skip crane's default zstd-pack-target step on install.
        doInstallCargoArtifacts = false;

        nativeBuildInputs = [
          pkgs.git
          pkgs.cacert
          bin
        ];
        buildPhaseCargoCommand = "loom --version";

        preCheck = ''
          export HOME=$(mktemp -d)
          export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
        '';
        # `--tree` (every verifier, no file filter) is the explicit scope
        # for a git-less build sandbox: the source artifact has no `.git`,
        checkPhaseCargoCommand = "loom gate check --tree";
      };
    in
    {
      checks = {
        inherit loom-gate-check;
      };
    };
}

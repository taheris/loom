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
        # so skip crane's default zstd-pack-target step on install. Saves
        # ~30s/rebuild for a 4.5 GiB target tree.
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
        checkPhaseCargoCommand = "loom gate check";
      };
    in
    {
      checks = {
        inherit loom-gate-check;
      };
    };
}

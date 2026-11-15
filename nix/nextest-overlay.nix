final: prev: {
  cargo-nextest = prev.cargo-nextest.overrideAttrs (
    {
      patches ? [ ],
      preUnpack ? "",
      buildInputs ? [ ],
      ...
    }:
    {
      patches = patches ++ [ ./patches/nextest-capture-handoff.patch ];
      preUnpack = (import ./cargo-home.nix) + preUnpack;
    }
    // final.lib.optionalAttrs final.stdenv.hostPlatform.isLinux {
      # The runner regression enables test-only TLS features that the upstream
      # distributor binary excludes. Keep its system AWS-LC dependency explicit.
      buildInputs = buildInputs ++ [ final.aws-lc ];
      doCheck = true;
      checkPhase = ''
        runHook preCheck
        target/${final.stdenv.hostPlatform.rust.rustcTarget}/release/cargo-nextest \
          nextest run --locked --cargo-profile release -p nextest-runner \
          -E 'test(capture_closes_while_unrelated_sibling_is_paused_before_exec)'
        runHook postCheck
      '';
    }
  );
}

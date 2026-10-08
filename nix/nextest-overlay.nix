final: prev: {
  cargo-nextest = prev.cargo-nextest.overrideAttrs (
    old:
    {
      patches = (old.patches or [ ]) ++ [ ./patches/nextest-capture-handoff.patch ];
    }
    // final.lib.optionalAttrs final.stdenv.hostPlatform.isLinux {
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

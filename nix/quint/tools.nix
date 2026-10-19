{ pkgs }:
let
  upstream =
    assert pkgs.quint.version == "0.33.0";
    pkgs.quint;
  evaluator = upstream.quint-evaluator.overrideAttrs (old: {
    preUnpack = (import ../cargo-home.nix) + (old.preUnpack or "");
    postPatch = (old.postPatch or "") + ''
      mkdir -p src/bin
      cp ${./checker.rs} src/bin/loom-quint-checker.rs
    '';
  });
in
pkgs.runCommand "loom-quint-tools-0.33.0"
  {
    nativeBuildInputs = [ pkgs.makeWrapper ];
    passthru = {
      inherit (upstream) quint-cli;
      inherit evaluator;
      version = "0.33.0";
    };
  }
  ''
    mkdir -p "$out/bin" "$out/share/quint/rust-evaluator-v0.7.0"
    makeWrapper ${pkgs.nodejs}/bin/node "$out/bin/quint" \
      --add-flags "--max-old-space-size=3072 ${upstream.quint-cli}/share/quint/dist/src/cli.js" \
      --set QUINT_HOME "$out/share/quint"
    ln -s ${evaluator}/bin/quint_evaluator "$out/share/quint/rust-evaluator-v0.7.0/quint_evaluator"
    makeWrapper ${pkgs.coreutils}/bin/timeout "$out/bin/loom-quint-checker" \
      --add-flags "--kill-after=5s 300s ${evaluator}/bin/loom-quint-checker" \
      --run 'ulimit -v 2097152'
  ''

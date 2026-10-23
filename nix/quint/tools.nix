{ pkgs }:
let
  checked = import ../verification.nix;
  resource =
    origin:
    checked.resource {
      root = ../..;
      inherit origin;
    };
  checker = resource "nix/quint/checker.rs";
  witnesses = resource "nix/quint/witnesses.rs";
  upstream =
    assert pkgs.quint.version == "0.33.0";
    pkgs.quint;
  evaluator = upstream.quint-evaluator.overrideAttrs (old: {
    preUnpack = (import ../cargo-home.nix) + (old.preUnpack or "");
    postPatch = (old.postPatch or "") + ''
      mkdir -p src/bin
      cp ${checker.path} src/bin/loom-quint-checker.rs
      cp ${witnesses.path} src/bin/loom-quint-witnesses.rs
    '';
  });
in
checked.check {
  resources = [
    checker
    witnesses
  ];
  evaluation = map resource [
    "nix/quint/tools.nix"
    "nix/verification.nix"
    "nix/cargo-home.nix"
    "rust-toolchain.toml"
  ];
  build =
    _:
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
        for binary in loom-quint-checker loom-quint-witnesses; do
          makeWrapper ${pkgs.coreutils}/bin/timeout "$out/bin/$binary" \
            --add-flags "--kill-after=5s 300s ${evaluator}/bin/$binary" \
            --run 'ulimit -v 2097152'
        done
      '';
}

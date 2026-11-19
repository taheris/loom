{ pkgs }:

pkgs.prek.overrideAttrs (
  {
    patches ? [ ],
    preUnpack ? "",
    postPatch ? "",
    ...
  }:
  {
    patches = patches ++ [ ./patches/prek-hook-status.patch ];
    preUnpack = (import ./cargo-home.nix) + preUnpack;
    # Installed hooks must not escape to Darwin's /bin/sh interpreter variant.
    # Keep the hook protocol and status patch unchanged; pin only its interpreter.
    postPatch = postPatch + ''
      substituteInPlace crates/prek/src/cli/install.rs \
        --replace-fail 'static HOOK_TMPL: &str = r#"#!/bin/sh' \
          'static HOOK_TMPL: &str = r#"#!${pkgs.runtimeShell}'
    '';
  }
)

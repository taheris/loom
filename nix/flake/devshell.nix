_:

{
  perSystem =
    {
      config,
      pkgs,
      wrapixLib,
      rustProfile,
      profileManifest,
      sandbox,
      loomBin,
      ...
    }:
    {
      devShells.default = wrapixLib.mkDevShell {
        shellHook = ''
          export CARGO_INCREMENTAL="''${CARGO_INCREMENTAL:-0}"
          export LOOM_PROFILES_MANIFEST=${profileManifest}
          export PATH="${rustProfile.toolchain}/bin:$PATH"
          export RUSTC_WRAPPER="${pkgs.sccache}/bin/sccache"
          export SCCACHE_CACHE_SIZE="''${SCCACHE_CACHE_SIZE:-50G}"
          export SCCACHE_DIR="''${SCCACHE_DIR:-$HOME/.cache/sccache}"
          if [[ -d .git ]]; then
            git config --local core.hooksPath lib/prek/hooks
          fi
          prek_home="''${PREK_HOME:-''${XDG_CACHE_HOME:-$HOME/.cache}/prek}"
          mkdir -p "$prek_home/tools/uv"
          ln -sfn "${pkgs.uv}/bin/uv" "$prek_home/tools/uv/uv"
        '';

        packages = [
          config.treefmt.build.wrapper
          loomBin
          pkgs.cargo-nextest
          pkgs.flock
          pkgs.prek
          pkgs.sccache
          pkgs.uv
          rustProfile.toolchain
          sandbox.package
        ];
      };
    };
}

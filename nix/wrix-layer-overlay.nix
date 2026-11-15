# Temporary Wrix-only substitution for dockerMakeLayers. Keep Nixpkgs' normal
# image construction and upstream tests unchanged; support only Wrix's pipeline.
final: _previous: {
  dockerMakeLayers =
    {
      closureRoots,
      excludePaths ? [ ],
      pipeline ? [ ],
      debug ? false,
    }:
    if closureRoots == [ ] then
      builtins.toFile "docker-layers-empty" "[]"
    else
      final.runCommand "wrix-lean-docker-layers"
        {
          __structuredAttrs = true;
          exportReferencesGraph.graph = closureRoots;
          exclude_paths = excludePaths;
          inherit pipeline debug;
        }
        ''
          set -euo pipefail
          ${
            import ./verification-tool.nix { pkgs = final; }
          }/bin/loom-verification layer-plan "$NIX_ATTRS_JSON_FILE" > "$out"
        '';
}

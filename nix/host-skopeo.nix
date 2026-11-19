# Archive conversion stays inside the caller's owned temporary directory.
{ pkgs }:
pkgs.writeShellScriptBin "skopeo" ''
  set -euo pipefail
  exec ${pkgs.skopeo}/bin/skopeo --tmpdir "''${TMPDIR:?Skopeo requires an owned temporary directory}" "$@"
''

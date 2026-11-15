# Exercise Loom's wrapper adaptation, not the upstream Beads/Dolt backend.
{ pkgs }:
let
  upstream = pkgs.writeShellScriptBin "bd" ''
    set -euo pipefail
    root=$(git rev-parse --show-toplevel)
    if [[ -n "$root" && ( -f "$root/.beads/metadata.json" || -f "$root/.beads/dolt/WRIX-MANAGED" ) ]]; then
      "''${WRIX_BIN:?fixture broker required}" service dolt client-env
    fi
    printf 'fixture bd version\n'
  '';
  sdk = import ../native-smoke-beads.nix { inherit pkgs upstream; };
  broker = pkgs.writeShellScript "native-beads-broker" ''
    set -euo pipefail
    [[ "$#" == 3 && "$1" == service && "$2" == dolt && "$3" == client-env ]]
    printf 'fresh\n' >> "$LOOM_TEST_BROKER_LOG"
  '';
in
pkgs.runCommand "native-smoke-beads"
  {
    nativeBuildInputs = [ pkgs.git ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR/home" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
    mkdir -p "$HOME" source
    cd source
    git init -q
    export WRIX_BIN=${broker} LOOM_TEST_BROKER_LOG="$TMPDIR/broker.log"
    # Negative control: unadapted external wrapper does not request credentials.
    ${upstream}/bin/bd --version > "$TMPDIR/upstream.version"
    test -s "$TMPDIR/upstream.version"
    test ! -e "$LOOM_TEST_BROKER_LOG"
    for marker in absent metadata; do
      if [[ "$marker" == metadata ]]; then
        mkdir .beads
        printf '{}\n' > .beads/metadata.json
      fi
      ${sdk}/bin/bd --version > "$TMPDIR/$marker.version"
      test -s "$TMPDIR/$marker.version"
    done
    printf 'fresh\nfresh\n' > "$TMPDIR/expected"
    cmp "$TMPDIR/expected" "$LOOM_TEST_BROKER_LOG"
    touch "$out"
  ''

{ pkgs, upstream }:
let
  sdk = import ../native-smoke-beads.nix { inherit pkgs upstream; };
  broker = pkgs.writeShellScript "native-beads-broker" ''
    set -euo pipefail
    [[ "$#" == 3 && "$1" == service && "$2" == dolt && "$3" == client-env ]]
    for name in WRIX_BEADS_MANAGED WRIX_BEADS_SYNC_ENDPOINT WRIX_BEADS_WORKTREE \
      BEADS_DOLT_SERVER_DATABASE BEADS_DOLT_SHARED_SERVER BEADS_DOLT_SERVER_USER \
      BEADS_DOLT_PASSWORD BEADS_DOLT_SERVER_SOCKET BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT; do
      if [[ -v "$name" ]]; then
        printf 'inherited tracker identity was not cleared: %s\n' "$name" >&2
        exit 1
      fi
    done
    printf 'fresh\n' >> "$LOOM_TEST_BROKER_LOG"
    printf 'export WRIX_BEADS_MANAGED=1\n'
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
    ${upstream}/bin/bd --version > "$TMPDIR/upstream.version"
    test -s "$TMPDIR/upstream.version"
    test ! -e "$LOOM_TEST_BROKER_LOG"
    for marker in absent metadata; do
      if [[ "$marker" == metadata ]]; then
        mkdir .beads
        printf '{}\n' > .beads/metadata.json
      fi
      WRIX_BEADS_MANAGED=1 WRIX_BEADS_SYNC_ENDPOINT=stale WRIX_BEADS_WORKTREE=stale \
        BEADS_DOLT_SERVER_DATABASE=stale BEADS_DOLT_SHARED_SERVER=stale \
        BEADS_DOLT_SERVER_USER=stale BEADS_DOLT_PASSWORD=stale \
        BEADS_DOLT_SERVER_SOCKET=stale BEADS_DOLT_SERVER_HOST=stale BEADS_DOLT_SERVER_PORT=1 \
        ${sdk}/bin/bd --version > "$TMPDIR/$marker.version"
      test -s "$TMPDIR/$marker.version"
    done
    printf 'fresh\nfresh\n' > "$TMPDIR/expected"
    cmp "$TMPDIR/expected" "$LOOM_TEST_BROKER_LOG"
    touch "$out"
  ''

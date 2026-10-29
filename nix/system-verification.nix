# Assembled scenarios execute as native sandboxed producers over a private source fixture.
{
  pkgs,
  system,
  loom,
}:
let
  checked = import ./verification.nix;
  evaluation =
    map
      (
        origin:
        checked.resource {
          root = ../.;
          inherit origin;
        }
      )
      [
        "nix"
        "flake.nix"
        "flake.lock"
        "rust-toolchain.toml"
      ];
in
{
  name,
  program,
  arguments ? [ ],
  nixRegistration ? null,
  runtime ? "nix",
}:
let
  inherit (pkgs.lib) optional optionalString escapeShellArg;
  runner = pkgs.writeShellApplication {
    name = "${name}-producer";
    runtimeInputs = [
      pkgs.bash
      pkgs.coreutils
      pkgs.git
    ]
    ++ optional (nixRegistration != null) (
      if runtime == "native-container" then
        import ./native-container-nix.nix { inherit pkgs; }
      else
        pkgs.nix
    )
    ++ pkgs.lib.optionals (runtime == "native-container") [
      pkgs.iproute2
      pkgs.util-linux
    ];
    text = ''
      out="''${out:?native report output required}"
      ${optionalString (runtime == "native-container") ''
        mount -t devpts -o newinstance,ptmxmode=0666,mode=0620,gid=0 devpts /dev/pts
        mount --bind /dev/pts/ptmx /dev/ptmx
        : "''${LOOM_NATIVE_CGROUP_PARENT:?missing owned native cgroup parent}"
        read -r -a delegated < "/sys/fs/cgroup$LOOM_NATIVE_CGROUP_PARENT/cgroup.controllers"
        printf '+%s ' "''${delegated[@]}" > "/sys/fs/cgroup$LOOM_NATIVE_CGROUP_PARENT/cgroup.subtree_control"
        mount -t cgroup2 none /sys/fs/cgroup
        mkdir -p /sys/fs/cgroup/launcher
        while IFS= read -r pid < /sys/fs/cgroup/cgroup.procs; do
          printf '%s\n' "$pid" > /sys/fs/cgroup/launcher/cgroup.procs
        done
        read -r -a controllers < /sys/fs/cgroup/cgroup.controllers
        printf '+%s ' "''${controllers[@]}" > /sys/fs/cgroup/cgroup.subtree_control
        ip link set dev lo up
      ''}
      work=$(mktemp -d)
      export HOME="$work/home"
      mkdir -p "$HOME" "$work/source"
      cp -r ${loom.stagedSrc}/. "$work/source/"
      chmod -R u+w "$work/source"
      cd "$work/source"
      export GIT_CONFIG_GLOBAL="$PWD/tests/fixtures/git/test-gitconfig"
      export GIT_CONFIG_SYSTEM=/dev/null
      unset WRIX_SIGNING_KEY WRIX_DEPLOY_KEY SSH_AUTH_SOCK GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
      ${optionalString (nixRegistration != null) ''
        unset NIX_CONFIG
        export NIX_REMOTE=local NIX_STORE_DIR=/nix/store NIX_USER_CONF_FILES=/dev/null
        export NIX_STATE_DIR="$work/nix-state" NIX_LOG_DIR="$work/nix-log"
        export NIX_CONF_DIR="$work/nix-conf"
        mkdir -p "$NIX_STATE_DIR" "$NIX_LOG_DIR" "$NIX_CONF_DIR"
        printf '%s\n' \
          'experimental-features = nix-command flakes' \
          'build-users-group =' \
          'require-sigs = true' \
          'sandbox = true' \
          'sandbox-fallback = false' \
          'allow-import-from-derivation = false' \
          'substituters =' \
          'builders =' > "$NIX_CONF_DIR/nix.conf"
        nix-store --load-db < ${nixRegistration}
        ${optionalString (runtime == "native-container") ''
          : "''${LOOM_NATIVE_NIX_REGISTRATION:?missing admitted native input registration}"
          nix-store --load-db < "$LOOM_NATIVE_NIX_REGISTRATION"
        ''}
      ''}
      git init --quiet -b main
      git add --all
      GIT_AUTHOR_DATE="@1 +0000" GIT_COMMITTER_DATE="@1 +0000" \
        git -c user.name="Native system fixture" -c user.email="system-fixture@example.invalid" \
        -c commit.gpgsign=false -c core.hooksPath=/dev/null commit --quiet -m "Seal isolated scenario source"
      status=0
      ${program} ${pkgs.lib.escapeShellArgs arguments} > "$out/scenario-stdout" 2> "$out/scenario-stderr" || status=$?
      printf '%s\n' "$status" > "$out/scenario-status"
      cat "$out/scenario-stdout" >&2
      cat "$out/scenario-stderr" >&2
      if [[ "$status" == 0 ]]; then
        outcome=passed
      else
        outcome=failed
      fi
      printf '{"target":"%s","outcome":"%s","execution":%s,"evidence":"actual isolated scenario; complete stdout, stderr and producer status retained"}\n' '${name}' "$outcome" ${
        escapeShellArg (
          builtins.toJSON {
            platform = system;
            platforms = [ system ];
            capabilities =
              if runtime == "native-container" then
                [
                  "host-kernel"
                  "container-engine"
                ]
              else
                [ ];
          }
        )
      }
      # Reserved skip codes require a grounded reason; retain raw status but report failure.
      if [[ "$status" != 0 ]]; then exit 1; fi
    '';
  };
  execution =
    if runtime == "native-container" && pkgs.stdenv.hostPlatform.isLinux then
      let
        kernel = import ./native-container-kernel.nix {
          inherit pkgs system;
          program = "${runner}/bin/${name}-producer";
        };
      in
      "${kernel}/bin/loom-native-kernel"
    else
      "${runner}/bin/${name}-producer";
in
checked.reported {
  inherit
    name
    system
    evaluation
    runtime
    ;
  bash = pkgs.bash;
  coreutils = pkgs.coreutils;
  resources = loom.testResources;
  program = execution;
  parser = "json-lines";
  targets = [ name ];
  tier = "system";
}

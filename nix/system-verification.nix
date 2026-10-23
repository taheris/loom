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
}:
let
  runner = pkgs.writeShellApplication {
    name = "${name}-producer";
    runtimeInputs = [
      pkgs.bash
      pkgs.coreutils
      pkgs.git
    ];
    text = ''
      out="''${out:?native report output required}"
      work=$(mktemp -d)
      export HOME="$work/home"
      mkdir -p "$HOME" "$work/source"
      cp -r ${loom.stagedSrc}/. "$work/source/"
      chmod -R u+w "$work/source"
      cd "$work/source"
      export GIT_CONFIG_GLOBAL="$PWD/tests/fixtures/git/test-gitconfig"
      export GIT_CONFIG_SYSTEM=/dev/null
      unset WRIX_SIGNING_KEY WRIX_DEPLOY_KEY SSH_AUTH_SOCK GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
      git init --quiet -b main
      git add --all
      GIT_AUTHOR_DATE="@1 +0000" GIT_COMMITTER_DATE="@1 +0000" \
        git -c user.name="Native system fixture" -c user.email="system-fixture@example.invalid" \
        -c commit.gpgsign=false -c core.hooksPath=/dev/null commit --quiet -m "Seal isolated scenario source"
      status=0
      ${program} ${pkgs.lib.escapeShellArgs arguments} > "$out/scenario-stdout" 2> "$out/scenario-stderr" || status=$?
      cat "$out/scenario-stdout" >&2
      cat "$out/scenario-stderr" >&2
      if [[ "$status" == 0 ]]; then
        outcome=passed
      elif [[ "$status" == 77 || "$status" == 78 ]]; then
        outcome=skipped
      else
        outcome=failed
      fi
      printf '{"target":"%s","outcome":"%s","evidence":"actual isolated scenario; complete stdout, stderr and producer status retained"}\n' '${name}' "$outcome"
      exit "$status"
    '';
  };
in
checked.reported {
  inherit name system evaluation;
  bash = pkgs.bash;
  coreutils = pkgs.coreutils;
  resources = loom.testResources;
  program = "${runner}/bin/${name}-producer";
  parser = "json-lines";
  targets = [ name ];
  tier = "system";
}

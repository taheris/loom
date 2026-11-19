#!/usr/bin/env bash
set -euo pipefail
[[ $# == 1 ]] || exit 64
pre_push_checks="$1"
work=$(mktemp -d)
retain_failure() {
    local status=$?
    if [[ "$status" != 0 ]]; then
        for log in "$work"/*.log; do
            if [[ -f "$log" ]]; then printf '%s\n' "$log" >&2; cat "$log" >&2; fi
        done
    fi
}
trap retain_failure EXIT
export HOME="$work/home" XDG_STATE_HOME="$work/state" XDG_CACHE_HOME="$work/cache"
export PREK_HOME="$work/prek"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
unset WRIX_SIGNING_KEY WRIX_DEPLOY_KEY SSH_AUTH_SOCK GIT_CONFIG_COUNT
mkdir -m 700 "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
ssh-keygen -q -t ed25519 -N '' -f "$HOME/key"
printf 'landing@example.invalid %s\n' "$(< "$HOME/key.pub")" > "$HOME/allowed-signers"
git init --quiet --bare --initial-branch=main "$work/origin.git"
# This test owns signed Git/hook composition. Native Nix execution has its
# own smoke coverage; use Gate's real tracked document verifier here.
mkdir -p "$work/source/docs"
cd "$work/source"
printf '/.loom/\n' > .gitignore
printf '# Landing fixture\n' > docs/README.md
git init --quiet --initial-branch=main
git config user.name 'Agent landing fixture'
git config user.email landing@example.invalid
git config gpg.format ssh
git config user.signingkey "$HOME/key"
git config gpg.ssh.allowedSignersFile "$HOME/allowed-signers"
git config commit.gpgsign true
git add --all
git commit --quiet -m 'Seed isolated landing fixture'
git remote add origin "$work/origin.git"
git push --quiet -u origin main
cat > .pre-commit-config.yaml <<YAML
fail_fast: true
repos:
  - repo: local
    hooks:
      - id: loom-gate-verify-files
        name: actual agent files verification
        entry: loom gate verify --files
        language: system
        stages: [pre-commit]
        require_serial: true
      - id: publication-environment
        name: required publication environment
        entry: bash -c '[[ ! -e "\$HOME/deny-publication" ]]'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
      - id: loom-gate-verify-diff
        name: actual agent publication verification
        entry: bash '$pre_push_checks' --hook-id loom-gate-verify-diff --hook-entry 'loom gate verify --diff' --append-push-range -- loom gate verify --diff
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
YAML
prek install --hook-type pre-commit --hook-type pre-push
for hook in pre-commit pre-push; do
    IFS= read -r interpreter < ".git/hooks/$hook"
    [[ "$interpreter" == '#!/nix/store/'*'/bin/bash' && -x "${interpreter:2}" ]]
done
printf '# Valid implementation\n' > docs/README.md
git add .pre-commit-config.yaml docs/README.md
git commit -m 'Implement the actual declared predicate' > "$work/commit.log" 2>&1
git verify-commit HEAD
git push origin main > "$work/push.log" 2>&1
head=$(git rev-parse HEAD)
[[ $(git --git-dir="$work/origin.git" rev-parse main) == "$head" ]]
grep -F 'actual agent files verification' "$work/commit.log"
grep -F 'actual agent publication verification' "$work/push.log"
printf '\0invalid project text\n' > docs/README.md
git add docs/README.md
if git commit -m 'Reject invalid predicate' > "$work/rejected-commit.log" 2>&1; then
    printf 'invalid predicate passed the configured commit hook\n' >&2
    exit 1
fi
[[ $(git rev-parse HEAD) == "$head" ]]
grep -F 'project text contains NUL' "$work/rejected-commit.log"
git restore --staged docs/README.md
printf '# Valid implementation\n' > docs/README.md
printf '\nSecond valid publication.\n' >> docs/README.md
git add docs/README.md
git commit -m 'Prepare another valid publication' > "$work/second-commit.log" 2>&1
git verify-commit HEAD
touch "$HOME/deny-publication"
if git push origin main > "$work/rejected-push.log" 2>&1; then
    printf 'failed publication requirement passed the configured pre-push chain\n' >&2
    exit 1
fi
[[ $(git --git-dir="$work/origin.git" rev-parse main) == "$head" ]]
grep -F 'required publication environment' "$work/rejected-push.log"
rm "$HOME/deny-publication"
git push origin main > "$work/second-push.log" 2>&1
[[ $(git --git-dir="$work/origin.git" rev-parse main) == "$(git rev-parse HEAD)" ]]
printf 'actual signed commits and configured commit/pre-push positive/negative controls passed\n'

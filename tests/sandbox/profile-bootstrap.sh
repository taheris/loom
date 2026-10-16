#!/usr/bin/env bash
set -euo pipefail

loom_bin="${1:?fixed Loom fixture driver is required}"
wrix_bin="${2:?fixed Wrix launcher is required}"
manifest="${3:?fixed provisioned manifest is required}"
bash_package="${4:?fixed builder package is required}"
coreutils_package="${5:?fixed symlink tool package is required}"
system="${6:?fixed host system is required}"
if [[ "${LOOM_INSIDE:-0}" == 1 ]]; then
  printf 'profile bootstrap requires the trusted driver lane; worker coverage remains unverified\n' >&2
  exit 77
fi
git_vars=$(git rev-parse --local-env-vars)
while IFS= read -r git_var; do
  if [[ -n "$git_var" ]]; then unset "$git_var"; fi
done <<< "$git_vars"
fixture=$(mktemp -d)
cleanup() {
  local status=$?
  if [[ "$status" == 0 ]]; then
    rm -rf "$fixture"
  else
    printf 'retained failed profile-bootstrap fixture: %s\n' "$fixture" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
export HOME="$fixture/home"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_CACHE_HOME="$HOME/.cache"
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
export WRIX_DEPLOY_KEY="$fixture/bootstrap"
export WRIX_SIGNING_KEY="$fixture/bootstrap-signing"
export BEADS_DOLT_SERVER_SOCKET="$fixture/absent-dolt.sock"
export BD_NO_DAEMON=1
export BD_DISABLE_METRICS=1
unset GIT_SSH_COMMAND GIT_SSH
unset LOOM_PROFILES_MANIFEST LOOM_PROVISIONED_PROFILES_MANIFEST
unset WRIX_UNSAFE_PODMAN_SOCKET WRIX_NETWORK WRIX_NETWORK_ALLOWLIST
unset WRIX_PI_AUTH_FILE ANTHROPIC_API_KEY OPENAI_API_KEY CLAUDE_CODE_OAUTH_TOKEN
mkdir -p "$HOME" "$fixture/project/src"
ssh-keygen -q -t ed25519 -N "" -f "$WRIX_DEPLOY_KEY"
ssh-keygen -q -t ed25519 -N "" -f "$WRIX_SIGNING_KEY"
git init -q --bare "$fixture/origin.git"
cd "$fixture/project"
git init -q -b main
git remote add origin "$fixture/origin.git"
git config user.name 'Isolated profile bootstrap'
git config user.email fixture@example.invalid
"$wrix_bin" init --offline --no-hooks --sign --key bootstrap
printf '.loom/\n.wrix/\n.beads/\n' > .gitignore
printf '[package]\nname="bootstrap-fixture"\nversion="0.0.0"\nedition="2024"\n' > Cargo.toml
printf 'pub fn fixture() {}\n' > src/lib.rs
cat > .pre-commit-config.yaml <<'HOOKS'
repos:
  - repo: local
    hooks:
      - id: profile-factory-fixture
        name: profile factory fixture
        entry: 'true'
        language: system
        stages: [pre-commit, pre-push]
        always_run: true
        pass_filenames: false
HOOKS
cat > loom.toml <<'CONFIG'
[loom]
git_hook_timeout_secs = 30
[loop.infra]
max_attempts = 2
[phase.default]
profile = "rust"
agent.backend = "direct"
CONFIG
cat > flake.nix <<EOF
{
  outputs = { self }: let
    retain = path: builtins.appendContext path { "\${path}" = { path = true; }; };
    shell = retain "$bash_package";
    tools = retain "$coreutils_package";
    profiles = retain "$manifest";
  in {
    packages."$system".profile-images = builtins.derivation {
      name = "isolated-profile-bootstrap";
      system = "$system";
      builder = "\${shell}/bin/bash";
      args = [ "-euc" ''"\${tools}/bin/ln" -s "\${profiles}" "\$out"'' ];
    };
  };
}
EOF
git add .
git commit -qm 'Publish isolated profile factory fixture'
git push -q origin main
"$loom_bin" init
expected_head=$(git rev-parse HEAD)
initial_head="$expected_head"
cp flake.nix "$fixture/factory.saved"

run_until_missing_tracker() {
  local name="$1"
  local status
  if timeout --kill-after=5s 90s "$loom_bin" loop lm-bootstrap-fixture > "$fixture/$name.log" 2>&1; then
    cat "$fixture/$name.log" >&2
    printf 'fixture unexpectedly resolved an absent tracker\n' >&2
    exit 1
  else
    status=$?
  fi
  if [[ "$status" != 1 ]]; then
    cat "$fixture/$name.log" >&2
    printf 'bootstrap fixture did not retain a bounded tracker failure: %s\n' "$status" >&2
    exit 1
  fi
  if ! grep -F "\`bd\` exited with status" "$fixture/$name.log"; then
    cat "$fixture/$name.log" >&2
    if [[ -f .loom/diagnostics/profile-provisioning.jsonl ]]; then
      cat .loom/diagnostics/profile-provisioning.jsonl >&2
    fi
    exit 1
  fi
}

run_until_missing_tracker cold
jq -e --arg head "$expected_head" '.source == $head and .exit_code == 0 and .coverage_admitted == false' \
  .loom/diagnostics/profile-provisioning.jsonl >/dev/null
jq 'with_entries(.value |= with_entries(.value |= del(.native_preflight)))' "$manifest" > "$fixture/legacy.json"
export LOOM_PROFILES_MANIFEST="$fixture/legacy.json"
run_until_missing_tracker legacy
[[ "$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)" == 2 ]]

registry=$(jq -r .registry .git/loom-workspace.json)
mv "$registry/association.json" "$registry/association.json.saved"
if "$loom_bin" loop lm-bootstrap-fixture > "$fixture/missing-authority.log" 2>&1; then
  printf 'missing fixture authority was reconstructed or bypassed\n' >&2
  exit 1
fi
[[ ! -e "$registry/association.json" ]]
[[ "$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)" == 2 ]]
printf '{}\n' > "$registry/association.json"
if "$loom_bin" loop lm-bootstrap-fixture > "$fixture/corrupt-authority.log" 2>&1; then
  printf 'corrupt fixture authority was reconstructed or bypassed\n' >&2
  exit 1
fi
[[ "$(cat "$registry/association.json")" == '{}' ]]
[[ "$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)" == 2 ]]
mv "$registry/association.json.saved" "$registry/association.json"
run_until_missing_tracker recovered
[[ "$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)" == 3 ]]
[[ "$(git rev-parse HEAD)" == "$expected_head" ]]

printf '\n# unadmitted fixture edit\n' >> flake.nix
if "$loom_bin" loop lm-bootstrap-fixture > "$fixture/unpublished.log" 2>&1; then
  printf 'unpublished factory bytes were executed\n' >&2
  exit 1
fi
grep -F 'operator must match the clean published commit' "$fixture/unpublished.log"
[[ "$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)" == 3 ]]
cp "$fixture/factory.saved" flake.nix

printf '{ outputs = { self }: throw "isolated factory unavailable"; }\n' > flake.nix
git add flake.nix
git commit -qm 'Exercise bounded isolated factory outage'
git push -q origin main
git -C .loom/integration fetch -q origin main
git -C .loom/integration merge -q --ff-only origin/main
expected_head=$(git rev-parse HEAD)
run_until_missing_tracker exhausted
jq -se --arg head "$expected_head" \
  '[.[] | select(.source == $head)] | length == 2 and all(.[]; .exit_code != 0 and .coverage_admitted == false)' \
  .loom/diagnostics/profile-provisioning.jsonl >/dev/null
[[ "$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)" == 5 ]]

cp "$fixture/factory.saved" flake.nix
git add flake.nix
git commit -qm 'Recover the fixed isolated factory'
git push -q origin main
git -C .loom/integration fetch -q origin main
git -C .loom/integration merge -q --ff-only origin/main
expected_head=$(git rev-parse HEAD)
run_until_missing_tracker resumed
[[ "$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)" == 6 ]]
git merge-base --is-ancestor "$initial_head" HEAD
git verify-commit HEAD

signature_attempts=$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)
integration_before_signature=$(git -C .loom/integration rev-parse HEAD)
printf '{ outputs = { self }: builtins.trace "UNAUTHENTICATED_FACTORY_EXECUTED" (throw "must not execute"); }\n' > flake.nix
git add flake.nix
git -c commit.gpgsign=false commit -qm 'Exercise unsigned unpublished-to-integration factory rejection'
git push -q origin main
unsigned_head=$(git rev-parse HEAD)
if git -C .loom/integration cat-file -e "$unsigned_head^{commit}" 2>/dev/null; then
  printf 'unsigned fixture must initially be absent from integration\n' >&2
  exit 1
fi
if "$loom_bin" loop lm-bootstrap-fixture > "$fixture/unsigned-source.log" 2>&1; then
  printf 'unsigned published operator factory was accepted\n' >&2
  exit 1
fi
grep -F 'published factory source has no authenticated commit signature' "$fixture/unsigned-source.log"
if grep -Fq UNAUTHENTICATED_FACTORY_EXECUTED "$fixture/unsigned-source.log"; then
  printf 'unsigned factory executed before authentication\n' >&2
  exit 1
fi
[[ "$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)" == "$signature_attempts" ]]
[[ "$(git -C .loom/integration rev-parse HEAD)" == "$integration_before_signature" ]]
cp "$fixture/factory.saved" flake.nix
git add flake.nix
git commit -qm 'Recover with a signed published operator factory'
git push -q origin main
expected_head=$(git rev-parse HEAD)
if git -C .loom/integration cat-file -e "$expected_head^{commit}" 2>/dev/null; then
  printf 'signed recovery must initially be absent from integration\n' >&2
  exit 1
fi
run_until_missing_tracker signed-source-recovery
[[ "$(wc -l < .loom/diagnostics/profile-provisioning.jsonl)" == "$((signature_attempts + 1))" ]]
[[ "$(git -C .loom/integration rev-parse HEAD)" == "$integration_before_signature" ]]
git -C .loom/integration fetch -q origin main
git -C .loom/integration merge -q --ff-only origin/main
printf 'published-profile-bootstrap-authenticates-unfetched-source-ok\n'
git -C .loom/integration config user.name 'Profile Bootstrap Fixture'
git -C .loom/integration config user.email bootstrap-fixture@example.invalid
printf 'unpublished candidate must remain preserved\n' > .loom/integration/pending-candidate.txt
git -C .loom/integration add pending-candidate.txt
git -C .loom/integration commit -qm 'Preserve an unpublished integration candidate'
pending_head=$(git -C .loom/integration rev-parse HEAD)
[[ "$pending_head" != "$expected_head" ]]
run_until_missing_tracker integration-ahead
[[ "$(git -C .loom/integration rev-parse HEAD)" == "$pending_head" ]]
jq -se --arg head "$expected_head" 'last | .source == $head and .exit_code == 0 and .coverage_admitted == false' \
  .loom/diagnostics/profile-provisioning.jsonl >/dev/null
printf 'published-profile-bootstrap-preserves-unpublished-integration-ok\n'
printf 'published-profile-bootstrap-authority-and-skew-ok\n'

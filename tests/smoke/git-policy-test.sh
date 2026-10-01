#!/usr/bin/env bash
set -euo pipefail

# Real Wrix and Git are required to exercise helper lookup and SSH signing.
wrix_bin="${1:?absolute Wrix launcher path is required}"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
export HOME="$fixture/home"
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
export WRIX_DEPLOY_KEY="$fixture/smoke"
export WRIX_SIGNING_KEY="$fixture/smoke-signing"
mkdir -p "$HOME" "$fixture/workspace"

test_smoke_git_tools_initialize_repository_signing() {
    cd "$fixture/workspace"
    ssh-keygen -q -t ed25519 -N "" -f "$WRIX_DEPLOY_KEY"
    ssh-keygen -q -t ed25519 -N "" -f "$WRIX_SIGNING_KEY"
    git init -q --bare "$fixture/origin.git"
    git init -q -b main
    git remote add origin "$fixture/origin.git"
    git config user.name "Smoke Git Policy"
    git config user.email smoke@example.com
    "$wrix_bin" init --offline --no-hooks --key smoke

    [[ "$(git config --local gpg.format)" == "ssh" ]]
    [[ "$(git config --local gpg.ssh.program)" == "wrix-git-sign" ]]
    [[ "$(git config --local commit.gpgsign)" == "true" ]]
    [[ "$(git config --local user.signingkey)" == "wrix/signing-key/smoke-signing" ]]
    [[ "$(git config --local gpg.ssh.allowedSignersFile)" == "wrix/allowed_signers" ]]
    [[ -s .git/wrix/allowed_signers && -x .git/wrix/git-ssh ]]
    git commit --allow-empty -q -m "Verify smoke repository signing"
    git verify-commit HEAD
}

test_transport_does_not_require_the_helper_shebang() {
    local ssh_command expected actual helper
    cd "$fixture/workspace"
    ssh_command=$(git config --local core.sshCommand)
    expected=$(bash -c "$ssh_command -G github.com")
    helper=.git/wrix/git-ssh

    {
        printf '#!%s/missing-interpreter\n' "$fixture"
        tail -n +2 "$helper"
    } > "$fixture/helper"
    cat "$fixture/helper" > "$helper"

    actual=$(bash -c "$ssh_command -G github.com")
    [[ "$actual" == "$expected" ]]
}

test_smoke_git_tools_initialize_repository_signing
test_transport_does_not_require_the_helper_shebang

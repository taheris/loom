# shellcheck shell=bash
set -euo pipefail

[[ "$#" -eq 5 && "$1" == init && "$2" == --offline && "$3" == --no-hooks && "$4" == --key ]]
[[ "$5" == "$(basename "${WRIX_DEPLOY_KEY:?}")" ]]
[[ -f "$WRIX_DEPLOY_KEY" && -f "${WRIX_SIGNING_KEY:?}" ]]

mkdir -p .git/wrix
public_key=$(ssh-keygen -y -f "$WRIX_SIGNING_KEY")
printf '%s %s\n' "${GIT_AUTHOR_EMAIL:?}" "$public_key" > .git/wrix/allowed_signers
printf '#!/bin/sh\nprintf "unexpected network access in Git-policy fixture\\n" >&2\nexit 1\n' > .git/wrix/git-ssh
chmod +x .git/wrix/git-ssh

git config --local core.sshCommand .git/wrix/git-ssh
git config --local gpg.format ssh
git config --local gpg.ssh.program wrix-git-sign
git config --local gpg.ssh.allowedSignersFile wrix/allowed_signers
git config --local user.signingkey "wrix/signing-key/$5-signing"
git config --local commit.gpgsign true

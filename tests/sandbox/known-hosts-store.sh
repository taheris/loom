#!/usr/bin/env bash
set -euo pipefail

installer="${1:?known-hosts installer is required}"
work=$(mktemp -d)
trap 'chmod -R u+rwX "$work"; rm -rf "$work"' EXIT
mkdir -p "$work/store/openssh/etc/ssh" "$work/root/etc"
printf 'Host *\n  StrictHostKeyChecking yes\n' > "$work/store/openssh/etc/ssh/ssh_config"
printf 'github.example ssh-ed25519 fixture\n' > "$work/known_hosts"
chmod -R a-w "$work/store"
ln -s "$work/store/openssh/etc/ssh" "$work/root/etc/ssh"

bash "$installer" "$work/known_hosts" "$work/root"
if [[ -L "$work/root/etc/ssh" || ! -w "$work/root/etc/ssh" ]]; then
    printf 'known-hosts installation leaves SSH configuration linked into the store\n' >&2
    exit 1
fi
cmp "$work/store/openssh/etc/ssh/ssh_config" "$work/root/etc/ssh/ssh_config"
cp "$work/root/etc/wrix/known_hosts_dir/known_hosts" "$work/root/etc/ssh/ssh_known_hosts"
cmp "$work/known_hosts" "$work/root/etc/ssh/ssh_known_hosts"
[[ ! -e "$work/store/openssh/etc/ssh/ssh_known_hosts" ]]
printf 'known-hosts-store-ok\n'

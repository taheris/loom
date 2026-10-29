#!/usr/bin/env bash
set -euo pipefail

policy="${1:?membership policy source required}"
fixture=$(mktemp -d)
operator="$fixture/operator"
git init -q -b main "$operator"
git -C "$operator" -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false \
    commit --allow-empty -qm initial
mkdir -p "$operator/.loom/beads"
git clone -q "$operator" "$operator/.loom/integration"
git clone -q "$operator" "$operator/.loom/beads/smoke-one"
git clone -q "$operator" "$fixture/unrelated"
operator_git=$(stat -c '%d:%i' "$operator/.git")
adapter="$fixture/adapter"
{
    printf '#!%s\nset -euo pipefail\n' "$(command -v bash)"
    printf 'export PATH=%q\noperator=%q\noperator_git=%q\nmembership=fixture\nnative_key=fixture\nwrix=false\n' "$PATH" "$operator" "$operator_git"
    cat "$policy"
} > "$adapter"
chmod 700 "$adapter"
request() {
    local path="$1" encoded
    encoded=$(printf '%s' "$path" | base64 -w0)
    jq -nc --arg path "$encoded" '{version:1,caller_directory_b64:$path}'
}
for checkout in "$operator" "$operator/.loom/integration" "$operator/.loom/beads/smoke-one"; do
    request "$checkout" | "$adapter" resolve > "$fixture/reply.json"
    jq -e --arg root "$(printf '%s' "$operator" | base64 -w0)" \
        --arg checkout "$(printf '%s' "$checkout" | base64 -w0)" \
        '.version == 1 and .membership == "fixture" and .primary_root_b64 == $root and .checkout_root_b64 == $checkout' \
        "$fixture/reply.json"
done
if request "$fixture/unrelated" | "$adapter" resolve > "$fixture/unrelated.log" 2>&1; then
    echo 'membership accepted unrelated Git clone with identical history' >&2
    exit 1
fi
mv "$operator/.loom/beads/smoke-one/.git" "$fixture/worker-git"
ln -s "$fixture/worker-git" "$operator/.loom/beads/smoke-one/.git"
if request "$operator/.loom/beads/smoke-one" | "$adapter" resolve > "$fixture/indirect.log" 2>&1; then
    echo 'membership accepted indirect worker Git storage' >&2
    exit 1
fi
mv "$operator/.git" "$fixture/operator-git"
git init -q -b main "$operator"
if request "$operator" | "$adapter" resolve > "$fixture/replaced.log" 2>&1; then
    echo 'membership accepted replacement operator Git identity' >&2
    exit 1
fi
printf 'membership controls retained at %s\n' "$fixture"

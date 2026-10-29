#!/usr/bin/env bash
set -euo pipefail

: "${operator:?explicit smoke operator required}"
: "${operator_git:?explicit operator Git identity required}"
: "${membership:?explicit smoke membership required}"
: "${native_key:?existing Wrix registration required}"
: "${wrix:?immutable Wrix launcher required}"

# The enclosing native smoke supplies an explicit, disposable host registration.
check_operator() {
    [[ "$(stat -c '%d:%i' "$operator/.git")" == "$operator_git" ]]
    [[ -d "$operator/.git" && ! -L "$operator/.git" ]]
}

check_checkout() {
    local checkout="$1" relative git_dir common
    check_operator
    [[ "$checkout" == "$(realpath -e "$checkout")" ]]
    if [[ "$checkout" != "$operator" && "$checkout" != "$operator/.loom/integration" ]]; then
        relative="${checkout#"$operator/.loom/beads/"}"
        [[ "$relative" != "$checkout" && -n "$relative" && "$relative" != */* ]]
    fi
    [[ -d "$checkout/.git" && ! -L "$checkout/.git" && ! -e "$checkout/.git/commondir" ]]
    git_dir=$(git -C "$checkout" rev-parse --absolute-git-dir)
    common=$(git -C "$checkout" rev-parse --path-format=absolute --git-common-dir)
    [[ "$git_dir" == "$checkout/.git" && "$common" == "$git_dir" ]]
}

if [[ "$#" == 1 && "$1" == resolve ]]; then
    request=$(jq -ce 'select(.version == 1 and (.caller_directory_b64 | type == "string"))')
    caller=$(jq -er '.caller_directory_b64' <<<"$request" | base64 --decode)
    checkout=$(git -C "$caller" rev-parse --show-toplevel)
    check_checkout "$checkout"
    before=$(stat -c '%d:%i' "$checkout/.git")
    checkout_b64=$(printf '%s' "$checkout" | base64 -w0)
    primary_b64=$(printf '%s' "$operator" | base64 -w0)
    check_checkout "$checkout"
    [[ "$before" == "$(stat -c '%d:%i' "$checkout/.git")" ]]
    jq -nc --arg member "$membership" --arg checkout "$checkout_b64" --arg primary "$primary_b64" \
        '{version:1,membership:$member,checkout_root_b64:$checkout,primary_root_b64:$primary}'
elif [[ "$#" == 4 && "$1" == --operator && "$2" == "$operator" && "$3" == --checkout ]]; then
    check_checkout "$4"
    before=$(stat -c '%d:%i' "$4/.git")
    "$wrix" workspace admit --integration loom-smoke --expect-membership "$membership" \
        --checkout "$4" --retain "$native_key" >/dev/null
    check_checkout "$4"
    [[ "$before" == "$(stat -c '%d:%i' "$4/.git")" ]]
else
    printf 'invalid smoke workspace membership invocation\n' >&2
    exit 1
fi

#!/usr/bin/env bash
set -euo pipefail

# Real bd is required to check metadata, label inheritance, and worker readiness.
seed_script=$(realpath "${1:?seed script path is required}")
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
export HOME="$fixture/home"
export XDG_CONFIG_HOME="$HOME/.config"
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
unset BEADS_DOLT_SERVER_SOCKET BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT
unset BEADS_DOLT_SERVER_MODE BEADS_DOLT_AUTO_START BEADS_DIR BEADS_DB
mkdir -p "$HOME" "$fixture/workspace"
cd "$fixture/workspace"
git init -q -b main
git config user.name "Smoke Fixture"
git config user.email smoke@example.com
git commit --allow-empty -q -m "Initialize fixture"
bd init --prefix=smoke --skip-hooks --skip-agents --non-interactive >/dev/null

bead_id=$(bash "$seed_script")
head=$(git rev-parse HEAD)
bd list --all --limit 0 --json >"$fixture/beads.json"
if ! jq -e --arg bead "$bead_id" --arg head "$head" '
    [.[] | select(.labels | index("loom:spec"))] as $specs |
    [.[] | select(.labels | index("loom:active"))] as $work |
    [.[] | select(.id == $bead)] as $tasks |
    length == 3 and
    ($specs | length) == 1 and
    ($work | length) == 1 and
    ($tasks | length) == 1 and
    $specs[0].issue_type == "epic" and
    $specs[0].status == "closed" and
    $specs[0].metadata["loom.todo_cursor"] == $head and
    ($specs[0].metadata | has("loom.base_commit") | not) and
    ($specs[0].labels | index("spec:smoke")) != null and
    $work[0].issue_type == "epic" and
    $work[0].status == "open" and
    $work[0].metadata["loom.base_commit"] == $head and
    ($work[0].labels | index("loom:spec")) == null and
    ($work[0].labels | index("spec:smoke")) != null and
    $tasks[0].issue_type == "task" and
    $tasks[0].status == "open" and
    $tasks[0].parent == $work[0].id and
    ($tasks[0].labels | index("profile:base")) != null and
    ($tasks[0].labels | index("spec:smoke")) != null and
    ($tasks[0].labels | index("loom:spec")) == null and
    ($tasks[0].labels | index("loom:active")) == null
' "$fixture/beads.json" >/dev/null; then
    printf 'invalid smoke spec/work fixture:\n' >&2
    jq . "$fixture/beads.json" >&2
    exit 1
fi
bd ready --json >"$fixture/ready.json"
if ! jq -e --arg bead "$bead_id" '
    [.[] | select(.issue_type != "epic")] |
    length == 1 and .[0].id == $bead
' "$fixture/ready.json" >/dev/null; then
    printf 'expected one ready smoke worker:\n' >&2
    jq . "$fixture/ready.json" >&2
    exit 1
fi

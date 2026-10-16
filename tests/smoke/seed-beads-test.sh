#!/usr/bin/env bash
set -euo pipefail

# Real bd is required to check metadata, label inheritance, and worker readiness.
seed_script=$(realpath "${1:?seed script path is required}")
smoke_spec=$(realpath "${2:?smoke spec path is required}")
export LOOM_TEST_ACCEPTANCE_HELPER="${3:?fixed smoke acceptance helper is required}"
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
mkdir -p specs docs
cp "$smoke_spec" specs/smoke.md
printf '# Docs\n\n- [smoke](../specs/smoke.md)\n' >docs/README.md
git add specs docs
git commit -q -m "Initialize fixture"
bd init --prefix=smoke --skip-hooks --skip-agents --non-interactive >/dev/null

bead_id=$(bash "$seed_script")
head=$(git rev-parse HEAD)
bd list --all --limit 0 --json >"$fixture/beads.json"
"$LOOM_TEST_ACCEPTANCE_HELPER" >"$fixture/acceptance.json"
if ! jq -e --arg bead "$bead_id" --arg head "$head" --slurpfile acceptance "$fixture/acceptance.json" '
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
    $tasks[0].metadata["loom.acceptance"] == $acceptance[0]["loom.acceptance"] and
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

# Validate real task JSON through the same acceptance resolver used before spawn.
bd show "$bead_id" --json >"$fixture/task.json"
"$LOOM_TEST_ACCEPTANCE_HELPER" --validate-task <"$fixture/task.json"
jq '.[0].metadata |= del(.["loom.acceptance"])' "$fixture/task.json" >"$fixture/unbound.json"
if "$LOOM_TEST_ACCEPTANCE_HELPER" --validate-task <"$fixture/unbound.json" >"$fixture/unbound.log" 2>&1; then
    printf 'unbound smoke task unexpectedly accepted\n' >&2
    exit 1
fi
grep -F 'smoke task lacks acceptance' "$fixture/unbound.log"
printf '\n## Success Criteria\n\n- A changed requirement. [check](true)\n' >specs/smoke.md
if "$LOOM_TEST_ACCEPTANCE_HELPER" --validate-task <"$fixture/task.json" >"$fixture/stale.log" 2>&1; then
    printf 'stale smoke task unexpectedly accepted\n' >&2
    exit 1
fi
grep -F 'ordinary task references no longer resolve' "$fixture/stale.log"
cp "$smoke_spec" specs/smoke.md
"$LOOM_TEST_ACCEPTANCE_HELPER" --validate-task <"$fixture/task.json"

# Native graph guards cannot be established by a capturing command runner.
work_root=$(jq -er '.[] | select(.labels | index("loom:active")) | .id' "$fixture/beads.json")
producer=$(bd create --silent --type task --parent "$work_root" --title "Decision producer" --description "Independent implementation")
choice=$(bd create --silent --type decision --status blocked --parent "$producer" --title "Decision candidate" --description "Canonical brief" --notes "Preserved human outcome")
bd show "$choice" --json >"$fixture/choice-before.json"
if bd dep add "$producer" "$choice" >"$fixture/descendant.stdout" 2>"$fixture/descendant.stderr"; then
    printf 'native descendant blocker unexpectedly accepted\n' >&2
    exit 1
fi
bd update "$choice" --parent "$work_root"
bd show "$choice" --json >"$fixture/reparent-checkpoint.json"
if ! jq -e --arg root "$work_root" --arg producer "$producer" '
    .[0].parent == $root and
    all(.[0].dependencies[]; .dependency_type != "parent-child" or .id != $producer)
' "$fixture/reparent-checkpoint.json" >/dev/null; then
    printf 'native parent update retained the old discovery hierarchy:\n' >&2
    jq . "$fixture/reparent-checkpoint.json" >&2
    exit 1
fi
if bd dep add "$choice" "$producer" --type discovered-from >"$fixture/legacy-discovery.stdout" 2>"$fixture/legacy-discovery.stderr"; then
    printf 'native dotted-ID discovery guard unexpectedly bypassed\n' >&2
    exit 1
fi
grep -q 'already a child' "$fixture/legacy-discovery.stderr"
choice=$(bd create --silent --type decision --status blocked --title "Work-root decision" --description "Canonical brief" --notes "Preserved human outcome")
bd show "$choice" --json >"$fixture/choice-before.json"
bd update "$choice" --parent "$work_root"
bd dep add "$choice" "$producer" --type discovered-from
bd show "$choice" --json >"$fixture/choice-after.json"
jq -e --arg root "$work_root" --arg producer "$producer" --slurpfile before "$fixture/choice-before.json" '
    .[0] as $after | $before[0][0] as $before |
    $after.parent == $root and
    (["id", "title", "description", "notes", "status", "metadata"] |
        all(. as $key | $after[$key] == $before[$key])) and
    any($after.dependencies[]; .id == $producer and .dependency_type == "discovered-from")
' "$fixture/choice-after.json" >/dev/null
bd dep add "$producer" "$choice"
if bd dep add "$choice" "$producer" >"$fixture/cycle.stdout" 2>"$fixture/cycle.stderr"; then
    printf 'native blocking cycle unexpectedly accepted\n' >&2
    exit 1
fi
proxy=$(bd create --silent --type task --parent "$producer" --title "Mixed-cycle control" --description "Native graph control")
if bd dep add "$choice" "$proxy" >"$fixture/mixed-cycle.stdout" 2>"$fixture/mixed-cycle.stderr"; then
    printf 'native mixed parent/blocking cycle unexpectedly accepted\n' >&2
    exit 1
fi
if bd dep add "$producer" smoke-missing-reference >"$fixture/missing.stdout" 2>"$fixture/missing.stderr"; then
    printf 'native invalid reference unexpectedly accepted\n' >&2
    exit 1
fi
second_choice=$(bd create --silent --type decision --status blocked --title "Second independent decision" --description "Canonical second brief")
bd update "$second_choice" --parent "$work_root"
bd dep add "$second_choice" "$producer" --type discovered-from
bd dep add "$producer" "$second_choice"
bd close "$choice" --reason "Human intent only"
bd ready --json >"$fixture/ready-partial.json"
jq -e --arg producer "$producer" 'all(.[]; .id != $producer)' "$fixture/ready-partial.json" >/dev/null
bd dep add "$choice" "$producer" --type discovered-from
bd update "$choice" --parent "$work_root"
bd show "$choice" --json >"$fixture/choice-replayed.json"
jq -e '.[0].status == "closed" and .[0].notes == "Preserved human outcome"' "$fixture/choice-replayed.json" >/dev/null
bd close "$second_choice" --reason "Second intent resolved"
bd close "$proxy" --reason "Finish isolated guard control"
bd show "$producer" --json >"$fixture/producer-after.json"
jq -e '.[0].status == "open"' "$fixture/producer-after.json" >/dev/null
bd ready --json >"$fixture/ready-after.json"
jq -e --arg producer "$producer" 'any(.[]; .id == $producer)' "$fixture/ready-after.json" >/dev/null

#!/usr/bin/env bash
set -euo pipefail

base_commit=$(git rev-parse HEAD)
spec_epic_id=$(bd create "smoke spec" \
    --description "container smoke spec metadata" \
    --type=epic --priority=2 \
    --labels="loom:spec,spec:smoke" \
    --metadata "{\"loom.todo_cursor\":\"$base_commit\"}" \
    --silent)
bd close "$spec_epic_id" --reason="spec metadata carrier" >/dev/null

molecule_id=$(bd create "smoke molecule" \
    --description "container smoke molecule" \
    --type=epic --priority=2 \
    --labels="loom:active,spec:smoke" \
    --metadata "{\"loom.base_commit\":\"$base_commit\"}" \
    --silent)

bd create "smoke happy-path" \
    --description "container smoke: pi happy-path" \
    --type=task --priority=2 \
    --labels="spec:smoke,profile:base" \
    --parent="$molecule_id" --no-inherit-labels \
    --silent

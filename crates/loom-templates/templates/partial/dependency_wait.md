## Dependency-Wait Marker

`LOOM_WAITING` is the loop-only terminal for pausing this open bead on work
owned by another bead. Before emitting it:

1. Declare the blocking edge with `bd dep add <current-bead> <blocker-bead>`.
2. Leave the current bead open; do **not** run `bd close`.
3. Ensure at least one declared blocker is still active (not closed).

Then end with the bare marker as the final non-empty line:

```text
LOOM_WAITING
```

The marker carries no JSON or prose payload because the Beads dependency graph
is the durable authority. A valid wait preserves this bead's workspace and
branch, skips integration and per-bead gates, consumes no retry budget, and
adds no blocked/clarify/infra label or status. The scheduler continues other
ready work; `bd ready` resurfaces this bead after its blockers close.

A marker on a closed bead or without an active declared blocking dependency is
invalid and enters recovery rather than silently parking the bead.

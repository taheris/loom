## Dependency-Wait Marker

`LOOM_WAITING` is a bare, unit terminal admitted in Loop and Todo. It pauses
open source work on actual unresolved prerequisites. For supported native,
non-descendant dependencies, before emitting it:

1. Declare the actual blocking edge with `bd dep add <source> <blocker>`.
2. Leave the source open; do **not** run `bd close`.
3. Ensure at least one declared blocker is still active (not closed).

Never fabricate a forbidden descendant edge or bypass native hierarchy guards.

{% if self.output_phase() == loom_protocol::output::Phase::Todo %}
Here the source is the injected work epic identified in Todo Batch above. Wait only when
**decomposition itself** cannot finish without that prerequisite. A decision
needed only by particular implementation tasks blocks those tasks, not the work
epic; report it and finish the exact roster with Todo instead. Keep the pending
work epic, children, fixed batch and drafts for resumption. Waiting does not
advance spec cursors, consume implementation notes, or activate the work epic.
{% else %}
Here the source is the current bead. Parent decisions under its owning work root,
preserve the discovering producer in candidate provenance, and report the complete
`LOOM_CLARIFY` inventory. Declare actual native prerequisites from the source to
those decision siblings. Never substitute a nonblocking reference or driver-only
hold for a native prerequisite. Workers must not reparent legacy decisions.
A valid wait preserves the workspace and branch and
skips integration and per-bead gates. The scheduler continues other ready implementation work rather
than redispatching this source to rediscover its blockers.
{% endif %}

```text loom-example
LOOM_WAITING
```

The marker carries no JSON or prose payload. Actual unresolved native Beads
prerequisites prove a wait; syntax or nonblocking references alone do not.
A valid wait consumes no retry budget and adds no human-queue label to the source.
After admission the driver records an attributed wait; supported native waits
park eligible work as `status=blocked`. The driver selectively releases only
its own attributed state when current
prerequisites resolve, never unrelated human blocking state, native blockers,
semantic/infra holds, metadata or history. Prerequisite release does not close
implementation work. A closed source or a source without an active declared or
admitted prerequisite is invalid and enters recovery.

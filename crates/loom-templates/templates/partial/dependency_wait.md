## Dependency-Wait Marker

`LOOM_WAITING` is a bare, unit terminal admitted in Loop and Todo. It pauses
open source work only on actual work owned by another bead. Before emitting it:

1. Declare the actual blocking edge with `bd dep add <source> <blocker>`.
2. Leave the source open; do **not** run `bd close`.
3. Ensure at least one declared blocker is still active (not closed).

{% if self.output_phase() == loom_protocol::output::Phase::Todo %}
Here the source is the injected work epic identified in Todo Batch above. Wait only when
**decomposition itself** cannot finish without that prerequisite. A decision
needed only by particular implementation tasks blocks those tasks, not the work
epic; report it and finish the exact roster with Todo instead. Keep the pending
work epic, children, fixed batch and drafts for resumption. Waiting does not
advance spec cursors, consume implementation notes, or activate the work epic.
{% else %}
Here the source is the current bead. A valid wait preserves its workspace and
branch and skips integration and per-bead gates. The scheduler continues other
ready work rather than redispatching this source to rediscover its blockers.
{% endif %}

```text loom-example
LOOM_WAITING
```

The marker carries no JSON or prose payload: Beads prerequisites are the durable
authority. A valid wait consumes no retry budget and adds no human-queue label
to the source. After admission the driver records an attributed wait and parks
eligible work visibly as `status=blocked`; it selectively releases only its own
wait when current prerequisites resolve, never unrelated human blocking state.
Dependency release does not close implementation work. A closed source or a
source without an active declared prerequisite is invalid and enters recovery.

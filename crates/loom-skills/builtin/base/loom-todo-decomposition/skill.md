---
name: loom-todo-decomposition
description: Produce small, testable, dependency-aware work beads from specs and issues.
metadata:
  loom:
    phases: ["todo"]
---
# Todo Decomposition

Break spec work into beads that each have a clear acceptance boundary, relevant profile labels, and verifier expectations. Add dependencies when ordering matters and keep independent work unblocked.

`loom todo` is a decomposition pass, not the implementation or review pass.
Use the injected diffs and criterion-status rows first, inspect only the
representative code needed to choose bead boundaries, and create an explicit
audit/implementation bead when broad missing or stale evidence would otherwise
require exhaustive investigation.

Before ending the session, verify that normal assistant text contains exactly
one terminal marker: `LOOM_TODO: <json>` for success, or the phase-appropriate
worker self-report marker for clarify/blocked. Never finish after only thinking
text or tool calls.

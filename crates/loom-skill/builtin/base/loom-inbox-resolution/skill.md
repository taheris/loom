---
name: loom-inbox-resolution
description: Resolve clarify, blocked, infra, and tune items through chat rather than host-side mutation menus.
metadata:
  loom:
    phases: ["inbox"]
---

# Inbox Resolution

Use chat to understand the human decision or infra diagnostic item, preserve
durable state in the bead or tune proposal, and avoid host-side shortcut
commands that bypass the conversation. Treat `loom:infra` as an operator
diagnostic rather than worker judgement, and make the final state explicit
before ending the session.

When unblocking child beads, follow the phase prompt's parent-aware resolution:
inspect actual parent-child links and include reopening the blocked owning work
epic in the same human-confirmed action. Verify both child and epic afterward.
Preserve unrelated siblings and dependencies, and distinguish reopening for
retry from resolving an independent blocker.

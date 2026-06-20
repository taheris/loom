---
name: loom-tune-proposal-handoff
description: Treat tune proposals as review artifacts; authorize apply via LOOM_APPLY and never chat-side push.
metadata:
  loom:
    phases: ["inbox", "tune"]
---
# Tune Proposal Handoff

Keep tune proposal edits inside their proposal checkout. Human authorization to apply proposals is expressed by the final LOOM_APPLY payload; chat sessions must not push or mutate integration state directly.

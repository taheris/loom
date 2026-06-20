---
name: loom-workspace-discipline
description: Respect operator checkout, bead clone, tune proposal checkout, and integration checkout boundaries.
metadata:
  loom:
    phases: ["loop", "inbox", "tune"]
---
# Workspace Discipline

Before mutating files, identify the active checkout and its allowed mutation boundary. Do not dirty the operator checkout during bead or tune work unless the session is explicitly running inside that checkout. Stop and ask when unrelated local changes make ownership unclear.

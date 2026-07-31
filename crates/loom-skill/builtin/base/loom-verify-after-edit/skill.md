---
name: loom-verify-after-edit
description: Run relevant verification after edits and report skipped or failed checks honestly.
metadata:
  loom:
    phases: ["loop", "tune"]
---
# Verify After Edit

After code or spec changes, run the smallest verifier set that can catch regressions for the touched surface, then expand when risk warrants it. Report exact commands, failures, and any skipped checks without implying unrun checks passed.

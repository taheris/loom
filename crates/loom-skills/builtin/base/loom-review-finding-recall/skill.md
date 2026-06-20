---
name: loom-review-finding-recall
description: Systematically check diffs against spec, style, and test expectations without dropping findings.
metadata:
  loom:
    phases: ["review", "gate"]
---
# Review Finding Recall

When reviewing, walk the diff against the relevant spec contracts, style rules, and verifier expectations. Keep each finding actionable, cite the violated rule or contract, and distinguish confirmed defects from speculative concerns.

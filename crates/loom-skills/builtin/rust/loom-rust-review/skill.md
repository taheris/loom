---
name: loom-rust-review
description: Review Rust diffs for correctness, error handling, async and process behavior, and public API drift.
metadata:
  loom:
    phases: ["review", "gate"]
    profiles: ["rust"]
---
# Rust Review

Check Rust changes for typed error propagation, ownership and lifetime soundness, async cancellation behavior, process IO edge cases, and public API compatibility. Confirm tests exercise the changed behavior.

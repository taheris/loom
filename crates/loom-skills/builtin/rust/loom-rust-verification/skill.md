---
name: loom-rust-verification
description: Prefer nix fmt, cargo build, cargo nextest run, and nix flake check as appropriate.
metadata:
  loom:
    phases: ["loop", "gate", "tune"]
    profiles: ["rust"]
---
# Rust Verification

Select Rust verifiers based on the touched surface. Run formatting after edits, targeted cargo tests during iteration, and broader workspace checks when public APIs, dependency graph, or cross-crate behavior changed.

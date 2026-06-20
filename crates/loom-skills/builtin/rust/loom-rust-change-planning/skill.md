---
name: loom-rust-change-planning
description: Plan Rust changes around module and API boundaries, ownership, and tests.
metadata:
  loom:
    phases: ["todo", "loop"]
    profiles: ["rust"]
---
# Rust Change Planning

Locate the owning module and public API before editing Rust code. Prefer typed boundaries, small modules, and tests that exercise behavior through the public seam rather than assertions on implementation text.

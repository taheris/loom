---
name: loom-rust-style-rules
description: Apply repo docs/style-rules.md, rustfmt, clippy, naming, and layout expectations.
metadata:
  loom:
    phases: ["loop", "review"]
    profiles: ["rust"]
---
# Rust Style Rules

Apply the repository Rust rules before finishing: parse at boundaries, use newtypes for identifiers, keep errors typed, avoid production panics, and preserve the nested module layout. Let rustfmt and clippy be the mechanical backstop.

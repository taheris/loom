# Loom Architecture

Loom is a Rust workflow orchestrator that drives an AI agent through a
spec-to-implementation pipeline. The full behavioural contract lives in
[`../specs/harness.md`](../specs/harness.md); this document is a brief
orientation.

## Design Principles

1. **Specs are the source of truth** — every phase reads `specs/<label>.md`;
   `loom todo` decomposes it into beads; `loom run` works one bead at a time.
2. **Typed primitives at the boundary** — IDs, events, and tool calls are
   newtypes (`BeadId`, `SpecLabel`, `MoleculeId`, …). Parse, don't validate.
3. **One spec lock at a time** — `<label>.lock` serializes spec-scoped phases;
   `workspace.lock` is held only during destructive state rebuild
   (`loom init --rebuild`).
4. **Verifiable annotations** — success criteria in specs carry
   `[verify]` / `[check]` / `[test]` / `[system]` / `[judge]` links so
   `loom gate verify` can run them deterministically.
5. **Backend-agnostic agent layer** — the `Session` trait lets pi-mono,
   Claude stream-json, and the Direct backend share the same workflow code.

## Repo Layout

```
.
├── Cargo.toml         # Workspace manifest
├── Cargo.lock
├── clippy.toml        # Workspace-wide clippy config
├── crates/            # Rust crates (see table below)
├── specs/             # Behavioural specifications
├── docs/              # Spec-authoring conventions, style rules
├── tests/
│   ├── mock-pi/       # Mock pi binary for protocol tests
│   ├── mock-claude/   # Mock claude binary for stream-json tests
│   └── judges/        # LLM judge rubrics ([judge] annotations)
└── flake.nix
```

## Crates

| Crate | Purpose |
|-------|---------|
| `loom` | CLI entry point and process plumbing |
| `loom-agent` | Backend abstraction: pi-mono RPC, Claude stream-json, and Direct |
| `loom-direct-runner` | Sandbox-aware tool runtime for the Direct backend |
| `loom-driver` | State store (SQLite), bd shim, lock manager, scratchpads, git client |
| `loom-events` | Typed event identifiers (`BeadId`, `SpecLabel`, `MoleculeId`, …) |
| `loom-gate` | Quality gate: `loom gate verify` (deterministic) + `loom gate review` (LLM judge) |
| `loom-llm` | Public-contract LLM primitives: `LlmClient`, `Conversation`, observers |
| `loom-render` | Streaming output formatters and event sinks |
| `loom-templates` | Askama prompt templates with typed contexts |
| `loom-test-support` | Shared test fixtures and helpers |
| `loom-walk` | Spec-annotation walker for `[verify]` / `[check]` / `[system]` |
| `loom-workflow` | Phase implementations: `plan`, `todo`, `run`, `gate`, `msg` |

## Phases

| Phase | Command | Lock | Inputs | Outputs |
|-------|---------|------|--------|---------|
| Plan (new) | `loom plan -n <label>` | `<label>.lock` | Project context | `specs/<label>.md`, state row |
| Plan (update) | `loom plan -u <label>` | `<label>.lock` | Existing spec, notes | Updated spec + notes |
| Todo | `loom todo <label>` | `<label>.lock` | Spec, molecule | New beads via `bd create` |
| Run | `loom run <label>` | `<label>.lock` | Beads, agent | Code changes, bead transitions |
| Gate (verify) | `loom gate verify` | none | Spec annotations | Deterministic pass/fail |
| Gate (review) | `loom gate review` | none | Diff, judge rubrics | LLM verdict |
| Msg | `loom msg <bead>` | per-bead | `loom:clarify` bead | Resolved options, label removed |

See [`../specs/harness.md`](../specs/harness.md) for the lock matrix and full
command set; [`../specs/gate.md`](../specs/gate.md) for the verification
model; and [`../specs/agent.md`](../specs/agent.md) for the backend
abstraction.

## State

Loom's state lives under `.wrapix/loom/` in the workspace:

```
.wrapix/loom/
├── state.db          # SQLite: specs, molecules, beads, notes, companions
├── workspace.lock    # Held by `loom init`
├── *.lock            # Per-spec locks, named after the SpecLabel
├── scratch/<key>/    # Per-session scratchpads (deleted at session end)
└── logs/<label>/     # Session transcripts, agent JSONL, gate output
```

The state DB is rebuildable from the workspace (`loom init --rebuild`) by
replaying `specs/*.md`, `bd list`, and git history — so it carries no
load-bearing information that doesn't already live in those sources.

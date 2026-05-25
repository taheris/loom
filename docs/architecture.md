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

## Crate Topology

```
crates/
├── loom/                    # CLI entry point, process plumbing
├── loom-driver/             # State store, locks, scratchpads, bd shim, git client
├── loom-events/             # Typed identifiers and event schema
├── loom-agent/              # Session trait + pi / claude / direct backends
├── loom-llm/                # Public LLM primitives (LlmClient, Conversation, observers)
├── loom-direct-runner/      # Sandbox-aware tool runtime for the Direct backend
├── loom-templates/          # Askama prompt templates with typed contexts
├── loom-gate/               # `loom gate verify` + `loom gate review`
├── loom-walk/               # [verify]/[check]/[system] annotation walker
├── loom-render/             # Streaming output formatters and event sinks
├── loom-workflow/           # Phase implementations (plan / todo / run / gate / msg)
└── loom-test-support/       # Shared fixtures and helpers
```

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

# Loom Architecture

Loom is a Rust workflow orchestrator that drives an AI agent through a
spec-to-implementation pipeline. The full behavioural contract lives in
[`../specs/harness.md`](../specs/harness.md); this document is a brief
orientation.

## Design Principles

1. **Specs are the source of truth** — spec epics retain per-spec metadata;
   `loom todo` decomposes changed specs into a work epic; `loom loop` executes
   its child beads, optionally in parallel.
2. **Typed primitives at the boundary** — IDs, events, and tool calls are
   newtypes (`BeadId`, `SpecLabel`, `MoleculeId`, …). Parse, don't validate.
3. **Independent work roots** — host-only locks serialize plan, todo, tune,
   initialization, and each mutating bead/work-epic root. Git's `index.lock`
   protects short integration operations; push races fetch, rebase, and re-gate.
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
| `loom-protocol` | Public-contract wire protocol types parsed by workflow, templates, and external consumers |
| `loom-llm` | Public-contract LLM primitives: `LlmClient`, `Conversation`, observers |
| `loom-render` | Streaming output formatters and event sinks |
| `loom-skill` | Public skill artifact model and registry stages |
| `loom-templates` | Askama prompt templates with typed contexts |
| `loom-test-support` | Shared test fixtures and helpers |
| `loom-tune` | Internal tuning registry, case, score, and proposal types |
| `loom-walk` | Spec-annotation walker for `[verify]` / `[check]` / `[system]` |
| `loom-workflow` | Phase implementations: `plan`, `todo`, `loop`, `gate`, `inbox` |

## Phases

| Phase | Command | Lock | Inputs | Outputs |
|-------|---------|------|--------|---------|
| Plan | `loom plan [SPEC_LABEL ...]` | `plan.lock` | Project context, spec index, optional anchors | Spec/index markdown + notes |
| Todo | `loom todo` | `todo.lock` | Changed specs, spec-epic cursors | New work epic and child beads |
| Loop | `loom loop [BEAD_OR_EPIC_ID ...]` | `<bead-or-epic-id>.lock` | Explicit roots or active work epic, agent | Code changes, bead transitions, gated integration |
| Gate (verify) | `loom gate verify` | none | Spec annotations | Deterministic pass/fail |
| Gate (review) | `loom gate review` | none | Diff, judge rubrics | LLM verdict |
| Inbox | `loom inbox` / `loom inbox chat` | targeted chat locks addressed bead | `loom:clarify`, `loom:blocked`, `loom:infra`, tune proposal beads | Human decision / diagnostic list/view/chat |
| Tune | `loom tune` subcommands | `tune.lock` for proposal allocation | Effective registry, cases, budget | Isolated replays, scored proposals for inbox review |

See [`../specs/harness.md`](../specs/harness.md) for the lock matrix and full
command set; [`../specs/gate.md`](../specs/gate.md) for the verification
model; and [`../specs/agent.md`](../specs/agent.md) for the backend
abstraction.

## State

Loom's state lives under `.loom/` in the workspace:

```
.loom/
├── cache.db          # SQLite: specs, work epics, notes, companions, evidence
├── beads/<id>/       # Disposable bead worktrees
├── integration/      # Publish-only integration checkout
├── tune/<id>/        # Tune proposal envelopes and candidate checkout
├── scratch/<key>/    # Per-session scratchpads (deleted at session end)
└── logs/             # Session transcripts, agent JSONL, gate/tune evidence
```

Advisory locks live outside container mounts at
`$XDG_STATE_HOME/loom/locks/<workspace-basename>/` (or the standard user-state
fallback), not under `.loom/`. Read-only inspection takes no lock.

The cache DB is rebuildable from the workspace (`loom init --rebuild`) by
replaying the spec index, spec files, bd epics, and git history — so it carries
no load-bearing information that doesn't already live in those sources.

## Compatibility and Retired Internals

Review recovery uses parsed `LOOM_FINDING` records and typed `PreviousFailure`
values. The never-populated workflow `ReviewFlag` / `review_flag` channel and
its verify-failure `review_notes` formatter have been removed. Direct template
callers can still supply `LoopContext.review_notes`; workflow drivers leave it
unset. The `ReviewConcern` display vocabulary also remains available.

`loom-protocol::gate::Finding` is a resolved, immutable value. External
consumers migrate direct literals or serde input to `RawFinding::resolve`,
and field reads to borrowed accessors; serialized JSON and finding IDs/hashes
are unchanged. Deterministic normalization also produces raw records until
they resolve against workspace declarations. `GateSuccess` likewise exposes
read-only accessors, closing post-construction mutation of validated evidence.
`WalkOutput`, `MarkerProof`, `VerifiedScope`, and `ReviewedScope` retain their
existing sealed boundaries; ordinary wire DTOs are not gate authorization.

Configuration parses phase keys and known values at ingestion; `agent_for`
now applies fallback infallibly. `BackendSettings` carries runtime-specific
settings, and checked suppressions expose only borrowed selectors/reasons.
Beads status/type/priority values are shared by response models and command
options, with unknown statuses/types rejected explicitly. Numeric/string wire
formats and creation defaults remain unchanged. Open `ModelName`/`ProviderName`
tokens live in `loom-events`; LLM fallback variants carry checked names instead
of arbitrary strings. `ModelId` parsing and Direct conversation construction
are fallible for malformed names, while valid unknown model routing is retained.

The following unused workspace-internal APIs have been retired:

- `GateError::Unimplemented`: implemented gate modules expose their own errors.
- `BdUpdateFn` and `CacheDb::consume_notes_and_refresh_base_commit`: current todo
  finalization owns durable metadata updates and compensation, then mirrors
  cursors/work state and consumes implementation notes via `CacheDb::finalize_todo`.
- `resolve_or_mint_open_epics`: the mint path uses the singular resolver.

No user command or agent wire alias is removed by this cleanup. In particular,
`loom use` and Pi's documented legacy `text` delta fields remain supported.

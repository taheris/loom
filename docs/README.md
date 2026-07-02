# Loom Docs

Specs live in [`../specs/`](../specs/). This index is pinned by `loom plan`
sessions.

## Authoring Conventions

- [`spec-conventions.md`](spec-conventions.md) — what a spec is and isn't,
  trust tiers, standard section structure. Pinned by `loom plan` sessions.
- [`style-rules.md`](style-rules.md) — code-style and test-quality rules
  organized by rule family (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-).
  Pinned by `loom loop` and `loom gate review` sessions.
- [`tuning.md`](tuning.md) — Loom tuning handbook: SkillOpt adaptation,
  behavioral checker model, `loom-case` syntax, and consumer tuning guidance.

## Specs

| Spec | Code | Epic | Purpose |
|------|------|------|---------|
| [agent.md](../specs/agent.md) | [`crates/loom-agent/`](../crates/loom-agent/) | `lm-4y0q` | Agent backend abstraction: pi-mono RPC, Claude Code stream-json, Direct (`loom-llm` + sandbox-aware tools via `loom-direct-runner`) |
| [events.md](../specs/events.md) | [`crates/loom-events/`](../crates/loom-events/) and [`crates/loom-render/`](../crates/loom-render/) | — | Typed event stream, command-wide live/replay rendering, LLM-visible transcript surface, persisted JSONL event logs, and diagnostic tracing boundary |
| [gate.md](../specs/gate.md) | [`crates/loom-gate/`](../crates/loom-gate/) | `lm-fbst` | Quality gate: conformance + style + test-quality dimensions, plan/per-diff/standing stages, `loom gate verify` (deterministic) + `loom gate review` (LLM judge) |
| [harness.md](../specs/harness.md) | [`crates/`](../crates/) | `lm-9ehh` | Platform: crate structure, workspace lints, process architecture, cache store, command set |
| [llm.md](../specs/llm.md) | [`crates/loom-llm/`](../crates/loom-llm/) | `lm-ywph` | Public-contract LLM primitives: `LlmClient`, typed `CacheControl`, `Conversation` with built-in tool-use loop, agent-loop observers (doom-loop, duplicate-result) |
| [pre-commit.md](../specs/pre-commit.md) | [`.pre-commit-config.yaml`](../.pre-commit-config.yaml) | `lm-q50m` | Hook composition policy: pre-commit (fast, ~1s) + pre-push (slow, ~10s + smoke) staged via `.pre-commit-config.yaml`; plumbing (lock, shim, install) delegated to `wrix.prekHooks` |
| [skills.md](../specs/skills.md) | [`crates/loom-skills/`](../crates/loom-skills/) plus planned internal crate `loom-tune` | — | Dynamic agent skill registry, built-in/profile-scoped skills, internal SkillOpt-style tuning engine, and `loom inbox` human review for tuned artifacts |
| [templates.md](../specs/templates.md) | [`crates/loom-templates/`](../crates/loom-templates/) | `lm-pe00` | Askama templates, partials inventory, per-phase pinning policy |
| [tests.md](../specs/tests.md) | [`tests/`](../tests/) | `lm-lsyj` | Test strategy: unit, integration, system tests |

## Terminology Index

| Term | Definition |
|------|------------|
| **bd** | CLI for the beads issue tracker |
| **Beads** | Persistent issue tracker used by Loom and the `bd` CLI |
| **AgentEvent** | Canonical typed event emitted by agent backends and the Loom driver; source of truth for command-wide live rendering, persisted JSONL logs, and replay |
| **agent_input** | `AgentEvent` variant emitted by the driver before Loom sends initial prompts, follow-up, steering, or re-pin text to an LLM-backed backend |
| **Event log** | Persisted JSONL copy of an `AgentEvent` stream under `.loom/logs/`, used by `loom logs` and external consumers |
| **JSONL** | JSON Lines — one complete JSON object per `\n`-terminated line; protocol framing for pi-mono RPC, Claude stream-json, Direct runner streams, and Loom event logs |
| **LLM-bearing command** | Non-interactive command path that spawns an agent backend, Direct conversation, LLM rubric, or tuning evaluator whose conversation is driven by Loom rather than an inherited interactive UI |
| **Loom** | Rust workflow orchestrator: spec-to-implementation pipeline with pi-mono, Claude Code, and Direct (loom-llm) backends |
| **loom:clarify** | Bead label for items awaiting human response via `loom inbox` |
| **loom:blocked** | Bead label for semantic worker/gate dead ends that need human resolution via `loom inbox`; distinct from generic `status=blocked` and from `loom:infra` diagnostics |
| **loom:infra** | Bead label for static or exhausted infrastructure / transport diagnostics; surfaced in `loom inbox` as kind `infra`, distinct from semantic `loom:blocked` |
| **Agent runtime** | Closed-set backend runtime selected by `agent.backend`: `pi`, `claude`, or `direct` |
| **Molecule** | Cross-cutting work grouping in Beads; Loom's CLI-facing decomposition container is the work epic for a changed-spec batch. |
| **pi** | Pi-mono stdio-RPC agent runtime; one backend Loom drives |
| **Profile** | Workspace toolchain axis (`base`, `rust`, `python`, …) paired with an agent runtime to select a sandbox image |
| **Scratchpad** | Per-session note file under `.loom/scratch/<key>/`, used for compaction recovery |
| **SessionId** | Stable event-session routing key carried on every `AgentEvent`; bead-backed sessions also carry `bead_id` |
| **Spec epic** | Durable per-spec Beads epic labelled `loom:spec` + `spec:<label>`; carries metadata such as `loom.todo_cursor` |
| **Skill** | Markdown agent capability package or loose skill file, identified by frontmatter `name` and progressively disclosed to agent backends |
| **Skill registry** | Effective per-session set of built-in, repo, configured, and override skills after profile/phase filtering and duplicate-name validation |
| **SpecLabel** | The kebab-case identifier matching a `specs/<label>.md` file |
| **Tune proposal** | Tune bead plus local `.loom/tune/<bead-id>/` envelope (`repo/`, manifest, evidence appendix) containing SkillOpt-style candidate edits awaiting human review through `loom inbox` |
| **Tuning case** | Strict TOML `loom-case` block in `docs/tuning.md` or package `tuning.md`, naming a built-in behavioral checker and explicit tune targets |
| **Workspace recovery stash** | Driver-created git stash preserving dirty `.loom/beads/<id>/` work before a `loom loop` dispatch; exposed to the worker as `workspace_recovery` context |
| **Work epic** | Per-`loom todo` decomposition batch epic; `loom:todo` while pending, `loom:active` when it is the default `loom loop` target |

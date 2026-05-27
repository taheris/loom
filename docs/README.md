# Loom Docs

Specs live in [`../specs/`](../specs/). This index is pinned by `loom plan`
sessions.

## Authoring Conventions

- [`spec-conventions.md`](spec-conventions.md) — what a spec is and isn't,
  trust tiers, standard section structure. Pinned by `loom plan` sessions.
- [`style-rules.md`](style-rules.md) — code-style and test-quality rules
  organized by rule family (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-).
  Pinned by `loom loop` and `loom gate review` sessions.

## Specs

| Spec | Code | Epic | Purpose |
|------|------|------|---------|
| [agent.md](../specs/agent.md) | [`crates/loom-agent/`](../crates/loom-agent/) | `lm-4y0q` | Agent backend abstraction: pi-mono RPC, Claude Code stream-json, Direct (`loom-llm` + sandbox-aware tools via `loom-direct-runner`) |
| [gate.md](../specs/gate.md) | [`crates/loom-gate/`](../crates/loom-gate/) | `lm-fbst` | Quality gate: conformance + style + test-quality dimensions, plan/per-diff/standing stages, `loom gate verify` (deterministic) + `loom gate review` (LLM judge) |
| [harness.md](../specs/harness.md) | [`crates/`](../crates/) | `lm-9ehh` | Platform: crate structure, workspace lints, process architecture, state store, command set |
| [llm.md](../specs/llm.md) | [`crates/loom-llm/`](../crates/loom-llm/) | `lm-ywph` | Public-contract LLM primitives: `LlmClient`, typed `CacheControl`, `Conversation` with built-in tool-use loop, agent-loop observers (doom-loop, duplicate-result) |
| [pre-commit.md](../specs/pre-commit.md) | [`.pre-commit-config.yaml`](../.pre-commit-config.yaml) | `lm-q50m` | Hook composition policy: pre-commit (fast, ~1s) + pre-push (slow, ~10s + smoke) staged via `.pre-commit-config.yaml`; plumbing (lock, shim, install) delegated to `wrapix.prekHooks` |
| [templates.md](../specs/templates.md) | [`crates/loom-templates/`](../crates/loom-templates/) | `lm-pe00` | Askama templates, partials inventory, per-phase pinning policy |
| [tests.md](../specs/tests.md) | [`tests/`](../tests/) | `lm-lsyj` | Test strategy: unit, integration, system tests |

## Terminology Index

| Term | Definition |
|------|------------|
| **bd** | CLI for the beads issue tracker |
| **Beads** | Persistent issue tracker used by Loom and the `bd` CLI |
| **JSONL** | JSON Lines — one complete JSON object per `\n`-terminated line; protocol framing for both pi-mono RPC and Claude stream-json |
| **Loom** | Rust workflow orchestrator: spec-to-implementation pipeline with pi-mono, Claude Code, and Direct (loom-llm) backends |
| **loom:clarify** | Bead label for items awaiting human response via `loom msg` |
| **Molecule** | A cross-cutting unit of work from one plan session: contains one epic per touched spec plus the tasks fanned out from each. At most one open epic per spec at a time. |
| **pi** | Anthropic's stdio-RPC agent runtime (pi-mono); one backend Loom drives |
| **Profile** | Image-manifest entry naming the sandbox image used for a given phase |
| **Scratchpad** | Per-session note file under `.wrapix/loom/scratch/<key>/`, used for compaction recovery |
| **SpecLabel** | The kebab-case identifier matching a `specs/<label>.md` file |

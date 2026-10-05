# Loom Docs

Specs live in [`../specs/<label>.md`](../specs/), with inline Success Criteria
and a 2,000-line ceiling per owner. This index is pinned by `loom plan`
sessions. The target `spec.md` / `tests.md` package layout follows
[tested tooling support](spec-conventions.md#bootstrap-authoring-before-cutover).
Ownership boundaries do not require new crates or commands.

## Authoring Conventions

- [`spec-conventions.md`](spec-conventions.md) — what a spec is and isn't, trust
  tiers, standard section structure. Pinned by `loom plan` sessions.
- [`style-rules.md`](style-rules.md) — code-style and test-quality rules
  organized by rule family (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-). Pinned
  by `loom loop` and `loom gate review` sessions.
- [`tuning.md`](tuning.md) — Loom tuning handbook: SkillOpt adaptation,
  behavioral checker model, `loom-case` syntax, and consumer tuning guidance.

## Specs

| Spec                                 | Code                                                                                                     | Epic      | Purpose                                                                                                                     |
| ------------------------------------ | -------------------------------------------------------------------------------------------------------- | --------- | --------------------------------------------------------------------------------------------------------------------------- |
| [agent](../specs/agent.md)           | [`crates/loom-agent/`](../crates/loom-agent/)                                                            | `lm-4y0q` | Pi, Claude, and Direct backend sessions, transports, launch protocols, and tools                                            |
| [events](../specs/events.md)         | [`crates/loom-events/`](../crates/loom-events/), [`crates/loom-render/`](../crates/loom-render/)         | —         | Typed events, live/replay rendering, transcripts, and ordinary-log retention                                                |
| [evidence](../specs/evidence.md)     | [`crates/loom-gate/`](../crates/loom-gate/)                                                              | —         | Result identity, provenance, freshness, reuse, retained failure/counterexample history, and recovery                        |
| [findings](../specs/findings.md)     | [`crates/loom-protocol/`](../crates/loom-protocol/), [`crates/loom-workflow/`](../crates/loom-workflow/) | —         | Immutable resolved findings, wire/terminal pairing, suppression, deduplication, and remediation                             |
| [gate](../specs/gate.md)             | [`crates/loom-gate/`](../crates/loom-gate/)                                                              | `lm-fbst` | Scope/stage composition, semantic review, publication receipts, markers, and attempt admission                              |
| [harness](../specs/harness.md)       | [`crates/`](../crates/)                                                                                  | `lm-9ehh` | Platform layering, configuration ingestion, typed subprocess/Beads boundaries, bootstrap, and cache infrastructure          |
| [inbox](../specs/inbox.md)           | [`crates/loom-workflow/`](../crates/loom-workflow/)                                                      | —         | Human queues, Options/decision briefs, clarification, diagnostics, and trusted tune-apply handoff                           |
| [llm](../specs/llm.md)               | [`crates/loom-llm/`](../crates/loom-llm/)                                                                | `lm-ywph` | Public typed completion, conversation, tools, cache controls, and observers                                                 |
| [loop](../specs/loop.md)             | [`crates/loom-workflow/`](../crates/loom-workflow/)                                                      | —         | Worker scheduling, reconciliation, bounded recovery, integration, and gated publication                                     |
| [plan](../specs/plan.md)             | [`crates/loom-workflow/`](../crates/loom-workflow/)                                                      | —         | Planning-only interviews, intent/coverage/coherence checks, sibling edits, and explicit commit consent                      |
| [pre-commit](../specs/pre-commit.md) | [`.pre-commit-config.yaml`](../.pre-commit-config.yaml)                                                  | `lm-q50m` | Repository hook composition, measured feedback, and Gate handoff through Wrix-owned plumbing                                |
| [simulation](../specs/simulation.md) | [`crates/loom-gate/`](../crates/loom-gate/), [`nix/`](../nix/)                                           | —         | Whole-gate Quint pilot in Loom and Wrix: reference equivalence, production conformance, finite campaigns, and cost evidence |
| [skills](../specs/skills.md)         | [`crates/loom-skill/`](../crates/loom-skill/)                                                            | —         | Skill artifacts, discovery, typed registry, filtering, overrides, and progressive disclosure                                |
| [specs](../specs/specs.md)           | [`crates/loom-gate/`](../crates/loom-gate/), [`crates/loom-templates/`](../crates/loom-templates/)       | —         | Canonical packages, parsing, criterion identity, snapshot resolution, and status projection                                 |
| [templates](../specs/templates.md)   | [`crates/loom-templates/`](../crates/loom-templates/)                                                    | `lm-pe00` | Askama composition, partials, explicit acceptance context, rendering, pinning, and compaction delivery                      |
| [tests](../specs/tests.md)           | [`tests/`](../tests/), [`crates/loom-test-support/`](../crates/loom-test-support/)                       | `lm-lsyj` | Shared test quality, deterministic fixtures, native suites, and assembled-system composition                                |
| [todo](../specs/todo.md)             | [`crates/loom-workflow/`](../crates/loom-workflow/)                                                      | —         | Changed-spec cursors/preflight, decomposition, typed handoff, Rust-owned task bindings, and finalization                    |
| [tuning](../specs/tuning.md)         | [`crates/loom-tune/`](../crates/loom-tune/)                                                              | —         | SkillOpt pipeline, cases/checkers, budgets, candidate validation, and proposal artifacts                                    |
| [verify](../specs/verify.md)         | [`crates/loom-gate/`](../crates/loom-gate/), [`crates/loom-walk/`](../crates/loom-walk/)                 | —         | Obligations, checked providers, tracked inputs, affected selection, batching, integrity, and execution                      |
| [workspaces](../specs/workspaces.md) | [`crates/loom-driver/`](../crates/loom-driver/)                                                          | —         | Checkout isolation, Git authority/signatures, profile images, mounts, locks, and dirty-work preservation                    |

## Terminology Index

| Term                         | Definition                                                                                                                                                                                   |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **bd**                       | CLI for the beads issue tracker                                                                                                                                                              |
| **Beads**                    | Persistent issue tracker used by Loom and the `bd` CLI                                                                                                                                       |
| **AgentEvent**               | Canonical typed event emitted by agent backends and the Loom driver; source of truth for command-wide live rendering, persisted JSONL logs, and replay                                       |
| **agent_input**              | `AgentEvent` variant emitted by the driver before Loom sends initial prompts, follow-up, steering, or re-pin text to an LLM-backed backend                                                   |
| **Event log**                | Persisted JSONL copy of an `AgentEvent` stream under `.loom/logs/`, used by `loom logs` and external consumers                                                                               |
| **JSONL**                    | JSON Lines — one complete JSON object per `\n`-terminated line; protocol framing for pi-mono RPC, Claude stream-json, Direct runner streams, and Loom event logs                             |
| **LLM-bearing command**      | Non-interactive command path that spawns an agent backend, Direct conversation, LLM rubric, or tuning evaluator whose conversation is driven by Loom rather than an inherited interactive UI |
| **Loom**                     | Rust workflow orchestrator: spec-to-implementation pipeline with pi-mono, Claude Code, and Direct (loom-llm) backends                                                                        |
| **loom:clarify**             | Bead label for items awaiting human response via `loom inbox`                                                                                                                                |
| **loom:blocked**             | Bead label for semantic worker/gate dead ends that need human resolution via `loom inbox`; distinct from generic `status=blocked` and from `loom:infra` diagnostics                          |
| **loom:infra**               | Bead label for static or exhausted infrastructure / transport diagnostics; surfaced in `loom inbox` as kind `infra`, distinct from semantic `loom:blocked`                                   |
| **Agent runtime**            | Closed-set backend runtime selected by `agent.backend`: `pi`, `claude`, or `direct`                                                                                                          |
| **Molecule**                 | Cross-cutting work grouping in Beads; Loom's CLI-facing decomposition container is the work epic for a changed-spec batch.                                                                   |
| **pi**                       | Pi-mono stdio-RPC agent runtime; one backend Loom drives                                                                                                                                     |
| **Profile**                  | Workspace toolchain axis (`base`, `rust`, `python`, …) paired with an agent runtime to select a sandbox image                                                                                |
| **Scratchpad**               | Per-session note file under `.loom/scratch/<key>/`, used for compaction recovery                                                                                                             |
| **SessionId**                | Stable event-session routing key carried on every `AgentEvent`; bead-backed sessions also carry `bead_id`                                                                                    |
| **Spec epic**                | Durable per-spec Beads epic labelled `loom:spec` + `spec:<label>`; carries metadata such as `loom.todo_cursor`                                                                               |
| **Skill**                    | Markdown agent capability package or loose skill file, identified by frontmatter `name` and progressively disclosed to agent backends                                                        |
| **Skill registry**           | Effective per-session set of built-in, repo, configured, and override skills after profile/phase filtering and duplicate-name validation                                                     |
| **SpecLabel**                | The stable kebab-case owner identifier; `specs/<label>.md` before cutover and the same label's canonical package afterward                                                                   |
| **Tune proposal**            | Tune bead plus local `.loom/tune/<bead-id>/` envelope (`repo/`, manifest, evidence appendix) containing SkillOpt-style candidate edits awaiting human review through `loom inbox`            |
| **Tuning case**              | Strict TOML `loom-case` block in `docs/tuning.md` or package `tuning.md`, naming a built-in behavioral checker and explicit tune targets                                                     |
| **Workspace recovery stash** | Driver-created git stash preserving dirty `.loom/beads/<id>/` work before a `loom loop` dispatch; exposed to the worker as `workspace_recovery` context                                      |
| **Publication attempt**      | Gate-admitted verification, review, and actual pre-push handoff for one publication request; not identified solely by unchanged Git fingerprints                                             |
| **Verification unit**        | An admitted execution mapped to obligations, with input identity and result provenance distinct from compiled build artifacts                                                                |
| **Work epic**                | Per-`loom todo` decomposition batch epic; `loom:todo` while pending, `loom:active` when it is the default `loom loop` target                                                                 |

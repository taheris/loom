# Prompt composition

Provides typed Askama contexts, partials, pinning, rendering, and compaction
delivery without owning workflow transitions.

## Problem Statement

Agents need the correct instructions and acceptance context at dispatch and
after compaction. Templates composes that context consistently without
reinterpreting task identity, evidence admission, or workflow transitions.

## Architecture

Typed workflow contexts feed compiled Askama templates and shared partials;
rendering and recovery preserve the supplied context. Related owners:
[specs](specs.md), [plan](plan.md), [todo](todo.md), [loop](loop.md),
[inbox](inbox.md), [findings](findings.md), [skills](skills.md),
[protocol](protocol.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

## Compaction Recovery

[Acceptance](#compaction-recovery-1).

Each agent-bearing session starts with a private `.loom/scratch/<key>/`
containing the full rendered `prompt.txt`, an empty append-only `scratch.md`, a
compact-session re-pin script, and any materialized built-in skills. Direct may
lazily add its output-offload directory. The key is the phase's concurrency
unit, so parallel beads do not share recovery state.

The initial prompt remains active instruction context after compaction. Backend
delivery reintroduces the full prompt before the live scratchpad, and
post-compaction output is not trusted until that pin is effective. Ordinary
conversation history yields before pinned protocol and mode instructions when a
backend limit forces a choice.
[Agent — Compaction Handling](agent.md#compaction-handling) owns delivery
mechanics.

The scratch tree is removed on every session exit and recreated empty on the
next session. It is a recovery aid, not durable workflow state.

### Template Files

[Acceptance](#success-criteria).

One template per agent-bearing phase:

- `plan.md`
- `todo.md`
- `loop.md`, `review.md`, `inbox.md`

`loom plan [SPEC_LABEL ...]` uses one planning template. The optional labels are
initial context anchors; new-vs-update is inferred from the spec/index files the
interview edits, not from separate template modes.

`loom todo` uses one decomposition template. The driver performs changed-spec
preflight, creates or reuses the `loom:todo` work epic, and injects the exact
changed-spec roster before the template renders; there is no `todo_new` /
`todo_update` split.

`loom gate verify` is deterministic — it runs project hooks, verifiers, audits,
and linters without rendering any agent prompt — so it has no template.
`loom gate review` is the LLM-judged counterpart and has its own template,
distinct from `loop.md` because the review session has different inputs (diff,
molecule/bead context, sibling diffs, typed deterministic gate evidence) and a
rubric-walk objective rather than an implement-the-bead objective.

Each template has a matching `#[derive(Template)]` context struct in the same
crate. The Askama build verifies every variable referenced in the template body
has a matching field on its context struct — missing variables are compile
errors.

### Partials

[Acceptance](#success-criteria).

Reusable fragments included via `{% include "partial/<name>.md" %}`. Current and
target v1 set; pending additions are marked in the pinning matrix:

| Partial                          | Purpose                                                                                                                                                                                                                                                                                |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `context_pinning.md`             | Pin the project-overview file (`pinned_context`)                                                                                                                                                                                                                                       |
| `style_rules.md`                 | Pin the style-rules file (`style_rules`) — see _Style-Rules Partial_ below                                                                                                                                                                                                             |
| `spec_conventions.md`            | Pin the spec-conventions document — see _Spec-Conventions Partial_ below                                                                                                                                                                                                               |
| `spec_header.md`                 | Render spec label/work-root context supplied by the phase                                                                                                                                                                                                                              |
| `companions_context.md`          | List companion paths declared on the spec(s) in scope                                                                                                                                                                                                                                  |
| `scratchpad.md`                  | Pin the per-session scratchpad path                                                                                                                                                                                                                                                    |
| `skill_index.md`                 | Render the precomputed compact index produced under [Skills — Registration and Progressive Disclosure](skills.md#registration-and-progressive-disclosure).                                                                                                                             |
| `progress_markers.md`            | Document `LOOM_COMPLETE` success and the loop-only `LOOM_NOOP` empty-diff success terminator. **Not pinned in `todo.md`** because todo success is the typed `LOOM_TODO:` payload, not a generic complete/no-op marker.                                                                 |
| `todo_success.md`                | Document the todo-specific success terminator `LOOM_TODO: <json>` and the `loom-protocol::todo::TodoSuccess` shape. Pinned only by `todo.md`.                                                                                                                                          |
| `self_report_markers.md`         | Present Loop/Todo retry/blocked outcomes with typed reasons and nonterminal decision-batch reporting backed by dedicated Beads briefs; defer message encoding and phase admission to Protocol.                                                                                         |
| `workspace_recovery.md`          | Loop-only recovery context for dirty bead workspaces saved to an unapplied git stash before dispatch; instructs the worker to inspect the stash before normal work and mention stash handling in its final summary.                                                                    |
| `review_self_report_markers.md`  | Document review-only cannot-complete terminators while preserving inspection-only review: no bd mutation instructions, and clarify-worthy decisions route through `route="clarify"` findings instead of direct `LOOM_CLARIFY`.                                                         |
| `options_format.md`              | Carry the canonical `## Options — <summary>` / `### Option N — <title>` markdown block consumed by Inbox chat, per [Inbox — Options Format Contract](inbox.md#options-format-contract).                                                                                                |
| `findings_walk.md`               | Canonical review presentation of finding payloads and explicit stream/concern pairing. Protocol owns message encoding; rendered conformance checks parsing/resolution, while the restatement audit prevents copied template prose.                                                     |
| `chat_marker_final_turn_only.md` | Restrict interactive-session terminal markers to the **final** assistant turn. `plan` may emit `LOOM_COMPLETE`; `inbox` may emit `LOOM_COMPLETE` or `LOOM_APPLY: {"proposals":[...]}`. Included by `plan` and `inbox`.                                                                 |
| `interview_modes.md`             | Describe the "one by one" / "polish the spec" interview sub-modes                                                                                                                                                                                                                      |
| `chat_interview.md`              | Interactive-session discipline for `plan` and `inbox`: conversational prose Q&A only, no Claude Code option-picker / `AskUserQuestion` widget, and phase-authorized durable destinations for anything that needs to outlive the session — see _Chat Discipline_ below                  |
| `decomposition_discipline.md`    | Pin the audit-before-fan-out and exact-roster rule on `todo`: every changed spec from driver preflight must be represented in `LOOM_TODO`, and every bead must correspond to evidence-confirmed missing work — see [Todo — Decomposition Discipline](todo.md#decomposition-discipline) |
| `dependency_wait.md`             | Present Loop/Todo prerequisite waiting: declare actual blockers, do not close waiting work, and explain driver-attributed visible parking, preservation, and selective resumption.                                                                                                     |
| `plan_stage_rubric.md`           | Gate the planning interview on completeness / coherence / invariant-clash before any commit. Carries the pending-modifier discipline prominently — see [Plan — Planning-Rubric Pending Discipline](plan.md#planning-rubric-pending-discipline).                                        |
| `invariant_clash.md`             | Describe the invariant-clash awareness scan (included transitively via `plan_stage_rubric.md`)                                                                                                                                                                                         |
| `review_rubric.md`               | Finite-diff / push-range review rubric — see [Gate](gate.md)                                                                                                                                                                                                                           |
| `sibling_spec_editing.md`        | Authorize cross-spec edits during a planning session                                                                                                                                                                                                                                   |

### Style-Rules Partial

[Acceptance](#success-criteria).

The `style_rules.md` partial is **rule-family-agnostic**: it instructs the agent
to discover rule families from the pinned `{{ style_rules }}` document, not from
a fixed prefix list. The template body never enumerates specific prefixes like
`RS-` or `COM-`; downstream consumers of loom maintain their own
`style-rules.md` with their own conventions, and the partial adapts.

The same agnosticism applies to the `review_rubric.md` partial in
[Gate](gate.md)'s style-rule-conformance dimension: the rubric instructs the
judge to walk every rule family the pinned document defines, without enumerating
prefixes. Any rule-ID example in template prose is illustrative (placeholder),
not normative.

### Spec-Conventions Partial

[Acceptance](#success-criteria).

The `spec_conventions.md` partial pins
[`docs/spec-conventions.md`](../docs/spec-conventions.md), which defines what a
spec is, what it isn't, and the relationship to code / verifiers / notes /
beads. Planning sessions read it so authored content complies with the
convention; this prevents implementation leakage, status indicators, and
historical narrative from drifting back into spec markdown.

### Pinning Policy

[Acceptance](#pinning-policy-1).

Each partial is included by an explicit set of templates. **Cell vocabulary**:
`✓` (partial is transitively `{% include %}`'d by this template), blank (partial
is NOT included), `?` (pending addition), `~` (pending removal). Pending cells
silent-pass during the pending window per
[Verify — Pending support in structured walker input](verify.md#pending-support-in-structured-walker-input).

| Partial                          | `plan` | `todo` | `loop` | `review` | `inbox` |
| -------------------------------- | :----: | :----: | :----: | :------: | :-----: |
| `context_pinning.md`             |   ✓    |   ✓    |   ✓    |    ✓     |    ✓    |
| `style_rules.md`                 |        |        |   ✓    |    ✓     |         |
| `spec_conventions.md`            |   ✓    |        |        |          |         |
| `spec_header.md`                 |   ?    |   ?    |   ✓    |    ✓     |         |
| `companions_context.md`          |   ✓    |   ✓    |   ✓    |    ✓     |    ✓    |
| `scratchpad.md`                  |   ✓    |   ✓    |   ✓    |    ✓     |    ✓    |
| `skill_index.md`                 |   ✓    |   ✓    |   ✓    |    ✓     |    ✓    |
| `progress_markers.md`            |   ✓    |        |   ✓    |    ✓     |         |
| `todo_success.md`                |        |   ✓    |        |          |         |
| `self_report_markers.md`         |        |   ✓    |   ✓    |          |         |
| `workspace_recovery.md`          |        |        |   ✓    |          |         |
| `review_self_report_markers.md`  |        |        |        |    ✓     |         |
| `findings_walk.md`               |        |        |        |    ✓     |         |
| `options_format.md`              |        |   ✓    |   ✓    |    ✓     |         |
| `chat_marker_final_turn_only.md` |   ✓    |        |        |          |    ✓    |
| `interview_modes.md`             |   ✓    |        |        |          |         |
| `chat_interview.md`              |   ✓    |        |        |          |    ✓    |
| `decomposition_discipline.md`    |        |   ✓    |        |          |         |
| `dependency_wait.md`             |        |   ?    |   ✓    |          |         |
| `plan_stage_rubric.md`           |   ✓    |        |        |          |         |
| `invariant_clash.md`             |   ✓    |        |        |          |         |
| `review_rubric.md`               |        |        |        |    ✓     |         |
| `sibling_spec_editing.md`        |   ✓    |        |        |          |         |

Pending cells mark planned include-graph updates whose prompt code has not
landed yet. The walker permits those cells while absent and reports them as
stale once the include graph catches up.

**Self-report guidance is phase-specific.** `self_report_markers.md` is pinned
only in `loop` and `todo`, where the agent persists dedicated decision briefs
before reporting their references independently of its final outcome. `review`
pins `review_self_report_markers.md` instead: review remains inspection-only, so
clarify-worthy decisions are emitted as `route="clarify"` findings with Options
in `evidence`, or as `LOOM_BLOCKED` when the reviewer cannot articulate options.

**`style_rules.md` is pinned only in `loop` and `review`** to support
implementation and quality review. This pinning policy does not prohibit code
inspection in other phases: [Todo](todo.md#decomposition-discipline) requires an
implementation/verifier audit before decomposition.

**`spec_conventions.md` is pinned only in `plan`** to guide specification
interviews. Pinning is not an edit-permission boundary: Loop also maintains
acceptance bindings under
[Verify's pending-marker lifecycle](verify.md#pending-modifier), including
removing a resolved marker in the implementing diff.

**`decomposition_discipline.md` and `todo_success.md` are pinned only in
`todo`** — the phase that authorizes bead creation. The driver has already
computed the changed-spec set; the prompt's job is to decompose that exact set,
report `Decomposed` or `NoWork` for every changed spec, and emit `LOOM_TODO:` as
the success marker.

### Agent-output conformance

[Acceptance](#agent-output-conformance-1).

[Protocol](protocol.md) supplies the canonical message vocabulary, role,
unit/data arity, phase projection, and strict framing. [Findings](findings.md)
supplies typed prompt-visible token, target, and scope metadata. These are
executable contract facts, not a second string registry maintained by Templates.
Small syntax/table/example fragments may be derived from that metadata; workflow
explanations remain narrative partials. Public composition continues to expose
the shared typed contexts and partials.

Every phase teaches only its admitted emit variants. Guidance distinguishes
nonterminal decision reporting from prerequisite waiting and completion, and
strict JSON from ordinary commentary. No partial teaches malformed-JSON repair,
terminal direct clarification, preceding-line reason/question scraping, or
required duplicate evidence narration. Review remains inspection-only.

Conformance tests render the real phase prompts with valid representative
context, explicitly extract their designated executable examples, and feed the
example bodies to the same production decoder and phase-admission path. Finding
examples also resolve against actual fixture files/specs/anchors and the real
workspace validator. The extraction convention is test-only: production never
promotes fenced prompt examples into agent messages. Pretty-printed payloads,
escaped multiline Options/evidence, and marker-looking payload text are
included.

Coverage checks enumerate each phase's permitted message variants and the
prompt-visible finding subset, including token/target alignment and scope
restrictions. Independent literal wire fixtures and negative/boundary cases
remain alongside generated examples and round trips, so matching mistakes in a
generator and parser cannot define correctness. Deliberate spelling, arity,
role, target, scope, and example drift must fail conformance. The template
restatement audit and text snapshots are complementary checks, not proof of
parser agreement or semantic review adequacy.

### Acceptance Context and Progressive Disclosure

[Acceptance](#success-criteria).

[Specs](specs.md#spec-packages) owns package discovery;
[spec conventions](../docs/spec-conventions.md#spec-packages) own document
roles. Templates deliver the applicable contract and acceptance obligations,
rather than unconditionally pinning every package's full `tests.md` and model.

- Planning receives the package contract and can retrieve detailed criteria.
- Decomposition receives relevant changed contracts, criteria, and evidence.
- Workers receive assigned acceptance obligations explicitly alongside the
  applicable contract; task acceptance is not optional supporting reading.
- Review receives applicable invariants, criteria, diff, and evidence.

When context selection is uncertain it broadens context rather than omitting
obligations. Models and supporting detail load on demand. Compaction recovery
restores the selected contract and explicit acceptance context under
[Compaction Recovery](#compaction-recovery), with backend delivery governed by
[Agent — Compaction Handling](agent.md#compaction-handling). Agents need not
rediscover their obligations.
[Loop's dispatch boundary](loop.md#task-acceptance-at-dispatch) supplies typed,
snapshot-resolved acceptance. For implementation tasks,
[Specs](specs.md#task-acceptance-references) supplies each obligation's
`(SpecLabel, CriterionId)`, full current text, and verifier binding. For
remediation, [Findings](findings.md#remediation-task-acceptance) supplies
resolved finding goals, attribution, targets, evidence, and processed-batch
acceptance. Templates render the supplied form, never reinterpret malformed
criteria as assignments or invent IDs for repair goals. They do not
independently parse references, resolve links, or omit unresolved members.
Compaction restores the dispatched goal set and task purpose, not references
reinterpreted against a different snapshot. Neither resolved form establishes a
verifier pass.

Decomposition renders both historical observations and the current coverage
projection supplied under [Specs](specs.md#criterion-status-surface) and
[Evidence](evidence.md#coverage-projection). It exposes blockers and uncertainty
rather than upgrading a cached `Current` pass or zero commit distance into
current coverage. Templates do not perform evidence admission.

### Template Variables

[Acceptance](#success-criteria).

Each top-level value rendered by a template or exposed on a workflow context is
bound to a typed field on the relevant context struct. `String`-typed values
arriving from beads or config flow through the parse-don't-validate boundary
defined in [Harness](harness.md#parse-dont-validate).

| Variable               | Type                           | Used By                                                                            |
| ---------------------- | ------------------------------ | ---------------------------------------------------------------------------------- |
| `pinned_context`       | `String`                       | all                                                                                |
| `style_rules`          | `String`                       | `loop`, `review`                                                                   |
| `spec_conventions`     | `String`                       | `plan`                                                                             |
| `anchor_labels`        | `Vec<SpecLabel>`               | `plan`                                                                             |
| `spec_index`           | `String`                       | `plan`, `todo`                                                                     |
| `label`                | `SpecLabel`                    | `loop`, `review`                                                                   |
| `spec_path`            | `String`                       | `loop`, `review`                                                                   |
| `changed_specs`        | `Vec<TodoChangedSpec>`         | `todo`                                                                             |
| `work_epic`            | `BeadId`                       | `todo`                                                                             |
| `todo_head`            | `GitSha`                       | `todo`                                                                             |
| `todo_fingerprint`     | `TodoFingerprint`              | `todo`                                                                             |
| `spec_epics`           | `Vec<SpecEpicContext>`         | `todo`                                                                             |
| `companion_paths`      | `Vec<String>`                  | `plan`, `todo`, `loop`, `review`, `inbox`                                          |
| `skill_index`          | `SkillIndexMarkdown`           | all agent-bearing templates                                                        |
| `implementation_notes` | `Vec<SpecImplementationNotes>` | `todo`                                                                             |
| `criterion_status`     | `Vec<CriterionStatus>`         | `todo` (see [Specs — Criterion-Status Surface](specs.md#criterion-status-surface)) |
| `inbox_items`          | `Vec<InboxItem>`               | `inbox`                                                                            |
| `molecule_id`          | `Option<MoleculeId>`           | `loop`, `review`                                                                   |
| `issue_id`             | `Option<BeadId>`               | `loop`                                                                             |
| `title`                | `Option<String>`               | `loop`                                                                             |
| `description`          | `Option<String>`               | `loop`                                                                             |
| `previous_failure`     | `Option<PreviousFailure>`      | `loop` (retry only; typed enum — see _Typed `PreviousFailure`_ below)              |
| `workspace_recovery`   | `Option<WorkspaceRecovery>`    | `loop` (dirty-workspace preservation only; see _Workspace-Recovery Surface_ below) |
| `review_notes`         | `Option<String>`               | `loop`                                                                             |
| `attempt`              | `u32`                          | `loop`                                                                             |
| `beads_summary`        | `Option<String>`               | `review`                                                                           |
| `base_commit`          | `Option<String>`               | `review`                                                                           |
| `test_sources`         | `Vec<ReviewSource>`            | `review`                                                                           |
| `judge_rubrics`        | `Vec<ReviewSource>`            | `review`                                                                           |
| `lane`                 | `ReviewLane`                   | `review`                                                                           |
| `default_profile`      | `ProfileName`                  | `review` (driver mint metadata; not rendered)                                      |
| `scratchpad_path`      | `String`                       | all                                                                                |

The newtypes (`SpecLabel`, `MoleculeId`, `BeadId`, `ProfileName`, `GitSha`,
`TodoFingerprint`, `CriterionId`) are architecture-bearing parse-boundary types.
`GitSha`, `TodoFingerprint`, and the todo success protocol live in
`loom-protocol::todo`; `SpecLabel`, `MoleculeId`, `BeadId`, and `ProfileName`
are defined in [Harness](harness.md#parse-dont-validate). The template treats
them as opaque typed values.

`implementation_notes` is sourced from `.loom/cache.db`'s `notes` table (kind =
`implementation`); see _Notes lifecycle_ in
[Harness](harness.md#sqlite-cache-store).

`skill_index` is generated by `loom-skill` after discovery, duplicate/override
resolution, phase/profile filtering, materialization, and backend disclosure
selection. The template layer receives a prompt-ready `SkillIndexMarkdown`
newtype rather than raw skill records; it renders the value through
`partial/skill_index.md` and does not inspect source/provenance. Native
registration status, source hashes, and override metadata are logged in
workflow/manifests, not rendered in normal prompts.

### Skill-Index Partial

[Acceptance](#success-criteria).

`partial/skill_index.md` is included by every agent-bearing template and renders
the prompt-ready `SkillIndexMarkdown` value unchanged. Disclosure modes, entry
contents, and path policy are owned by
[Skills — Registration and Progressive Disclosure](skills.md#registration-and-progressive-disclosure).
The partial adds only the template-owned boundary reminder that skill guidance
cannot override phase protocol, terminal markers, or gate requirements.

### Workspace-Recovery Surface

[Acceptance](#success-criteria).

`workspace_recovery` is loop-only prompt context produced when the driver
preserved dirty bead-workspace state before dispatch. It is separate from
`previous_failure`: it does not mean the prior worker failed, and it does not
increment or render the retry `attempt` counter. Both contexts may appear in the
same `loop.md` prompt; `previous_failure` renders first as failure context, then
`workspace_recovery` renders as the before-editing instruction to inspect
preserved local work. The partial is a recovery hint plus accountability
surface, not a typed terminal handoff.

```rust
pub struct WorkspaceRecovery {
    pub pre_stash_status: String,
    pub stash: RecoveryStash,
    pub integration_tip: GitSha,
    pub alignment: WorkspaceAlignment,
}

pub struct RecoveryStash {
    pub selector: String,     // e.g. "stash@{0}" at dispatch time
    pub commit: GitSha,      // stable stash commit for show/apply commands
    pub message: String,
}

pub enum WorkspaceAlignment {
    Clean,
    Rebased { previous_head: GitSha, current_head: GitSha },
    Conflict { files: Vec<String> },
}
```

`partial/workspace_recovery.md` renders only when this value is present. It
tells the worker to inspect the saved work before normal implementation using
the stable stash commit (`git stash show --stat <commit>` and
`git stash show -p <commit>`), then intentionally apply/cherry-pick, leave, or
drop the stash. When `alignment` is `Conflict`, the partial frames that as
agent-owned merge-conflict recovery: inspect the conflict files, resolve and
continue/abort/retry the rebase as appropriate, or persist dedicated decision
briefs and report their references through nonterminal `Clarify` records if the
conflict needs human decisions. The separate terminal reflects whether the
source is genuinely waiting, blocked without safe options, or able to finish.

The worker's normal prose summary before `LOOM_COMPLETE` should include one line
naming how the recovery stash was handled (applied, left for follow-up, not
relevant, or needs clarification) with brief rationale. The driver does not
parse that line and does not reject `LOOM_COMPLETE` solely because the stash
still exists; review can raise a concern if the agent ignored relevant preserved
work.

### Typed `PreviousFailure`

[Acceptance](#typed-previousfailure-1).

**Caps:**

- `PREVIOUS_FAILURE_MAX_LEN = 4000` total
- Each `VerifierFailure.stderr_tail` capped individually (~1500 chars) before
  the per-variant total is split across multiple failures (later failures
  truncated first when the total exceeds budget)
- `review_notes` remains a caller-supplied compatibility field; direct template
  callers own its budget (about 1000 chars is recommended). Workflow drivers
  leave it unset and carry reviewer evidence through typed `PreviousFailure`
  findings.

**Template framing.** Each variant renders distinctly:

- `DriverNotice` → `"Previous attempt: {detail}"`
- `VerifyFailures` →
  `"Verifier failures from previous attempt:\n\n{N blocks: target + exit + stderr}"`
- `ReviewConcern` →
  `"Review raised a concern ({label}): {summary}\n\n{per-finding digest: token + target + evidence}"`,
  where `{label}` is derived from the streamed finding tokens as described above
- `BadWalk(Concern { payload, parsed_findings })` →
  `"Your LOOM_CONCERN payload did not parse as {\"summary\": \"<non-empty>\"}. Literal payload: {payload}"`,
  followed (when `parsed_findings` is non-empty) by
  `"\n\n{N} finding(s) parsed cleanly before the malformed terminator:\n{per-finding digest: token + first line of evidence}"`
  so the agent's diagnosis from the streamed findings is not lost when only the
  terminal was malformed.
- `BadWalk(ConcernWithoutFindings { summary })` →
  `"You emitted LOOM_CONCERN ({summary}) but no LOOM_FINDING records streamed. Either emit findings before the terminator or use LOOM_COMPLETE."`
- `BadWalk(FindingsWithoutConcern { finding_count, findings })` →
  `"You streamed {finding_count} LOOM_FINDING record(s) but terminated with LOOM_COMPLETE. Use LOOM_CONCERN: {\"summary\": \"...\"} when findings are emitted."`,
  followed by `"\n\nFindings streamed:\n{per-finding digest}"` so the agent's
  next iteration sees the diagnosis it just emitted.
- `BadWalk(MalformedFinding { errors, terminal, parsed_findings })` → a
  strict-JSON correction followed by each raw/typed record error, the
  independently established or malformed/missing terminal, and a digest of the
  valid findings retained from the mixed stream. Guidance asks for column-one,
  unfenced messages and correctly escaped strings, not raw-newline repair.
  Terminal rendering derives from Protocol's canonical typed message; diagnostic
  malformed/missing surfaces preserve their literal context rather than another
  marker vocabulary. Shared framing/phase failures likewise show retained
  context categories within the existing budget.
- `BuildFailure` → `"Build failed at {stage}:\n{output}"`
- `TreeNotClean` →
  `"Working tree was not clean after the bead committed:\n\n{path list, one per line}\n\nStage these into a follow-up commit or revert them."`
  with a `"+N more"` suffix line when the list is truncated to 30 entries
- `PostIntegrateFail { failures, gate_log_path }` →
  `"After rebasing onto the integration branch, the post-integration verify failed.\n\nGate log: {gate_log_path}\n\n{N blocks: target + exit + stderr}\n\nReconcile the cross-bead interaction — your bead's verify passed at its own workspace; the failure is in the integrated tree."`
- `IntegrationConflict { files, new_base_sha }` →
  `"Your bead branch could not be rebased onto the integration branch — files conflict: <files>. The new integration tip is <new_base_sha>. Rebase your bead workspace onto the new tip, resolve, and re-commit."`
- `AgentRetry { reason }` → the decoded prior reason plus bounded recovery
  guidance: frame newly discovered decisions on dedicated beads and report their
  references, then wait only on genuine prerequisites; use a typed blocked
  reason when no safe options can be framed. Decision reporting is not another
  terminal.
- `review_notes` (when set, after the primary block) → heading `"Review notes:"`
  then content

Driver maps verdict-gate causes to variants per the table in
[Loop — Verdict Gate](loop.md#verdict-gate).

### First-instruction reframe

[Acceptance](#first-instruction-reframe-1).

When `attempt > 0 && previous_failure.is_some()`, `loop.md` prepends to its
first user instruction:

> "Re-read the previous failure block above and address its specific concern
> before re-implementing."

This single generic reframe forces the agent to acknowledge the prior failure as
actionable input rather than skim past it. The per-variant framing (above)
carries the cause-specific detail; the top-of-prompt reframe just establishes
the directive.

### Agent-Output Markers

[Acceptance](#agent-output-markers-1).

Agent-generated content rendered back into a prompt (`previous_failure`,
`title`, `description`, prior work-epic diagnostics, implementation notes) is
delimited with `<agent-output>` / `</agent-output>` markers so the receiving
agent can distinguish injected content from system instructions. This is a
best-effort prompt-injection mitigation; the real trust boundary is the
container.

### Chat Discipline

[Acceptance](#chat-discipline-1).

`partial/chat_interview.md` is included by every interactive-session template:
`plan.md` and `inbox.md`. It carries the discipline shared across every
interactive session the loom binary runs with a human in the loop:

- Questions go out in prose, in the assistant's normal reply. Answers come back
  as user prose.
- The agent does **not** use Claude Code's structured option-picker tool
  (`AskUserQuestion` or any equivalent multi-choice widget) for interactive
  sessions. The picker forces premature commitment to N enumerated options when
  the user's real answer may be a hybrid, a redirection, or none-of-the-above;
  it also adds friction to the short text replies that are the natural shape of
  conversational discussion.
- When the agent wants to propose alternatives, it lists them inline in prose
  ("option A does X; option B does Y"). The user replies "B" or "B with a tweak"
  or "neither, do Z" — natural prose, no picker UI.
- **Persistence destinations.** Session-bridging memory — decisions, context,
  follow-ups, anything future sessions need — goes only to the durable surface
  this phase authorizes. In `loom plan`, durable planning output goes in
  spec/index markdown or implementation notes; plan does not write bd. In
  `loom inbox`, bd notes/descriptions are the authorized resolution surface.
  Claude Code's `MEMORY.md` / auto-memory system is container-local and
  disappears with the container; treat it as working notes for the current
  session only, not as durable storage.
- The "one by one" sub-mode follows
  [Plan — Interview Modes](plan.md#interview-modes), is planning-specific, and
  lives in a separate partial; the chat-discipline rules above apply to every
  interactive session, including inbox-chat.

Inbox begins investigating a single visible item immediately, without asking
which item to start with or seeking permission to investigate. With multiple
visible items, it asks which item to start with before investigating that item.

Before asking the human to choose, Inbox proactively supplies a self-contained
decision brief: explain the problem in plain language and the decision needed,
compare what the options change and their practical benefits, costs, and risks,
and recommend an option with rationale and its main downside. Clarify items
retain their existing option numbers and titles rather than regenerating a menu
or merely repeating headings. When evidence is insufficient, the agent explains
the missing information and next investigation instead of guessing; persistence
still waits for human confirmation.

Worker phases (`loop`, `todo`, `review`) are single-shot and do not interview
the user, so the partial is not pinned there.

### Review Emit Shape

[Acceptance](#review-emit-shape-1).

`review.md` is the LLM-rubric walk's prompt template. It includes
`partial/findings_walk.md`, which is the sole agent-facing textual definition of
the review finding stream, terminal pairing rule, target shapes, routing fields,
and clarify Options requirements. The typed Rust contract and minting lifecycle
are owned by
[Findings — Findings and Minting](findings.md#findings-and-minting-1); this spec
owns only the template include relationship and the prompt-side mutation
boundary.

**The review template authorizes no bd writes.** The reviewing agent reports
findings through the included review-walk partial. Trusted driver recording and
materialization follow
[Findings' inspection/act boundary](findings.md#inspection-vs-act-partition),
including Loop's molecule-review routing and the explicit `loom gate mint`
command. Deduplication and lead-spec batching under a work epic belong to
[Findings](findings.md#deferred-remediation-processing), not the prompt. A
reviewing agent that mutates bd state violates the protocol.

**Clarify-bound findings embed Options in evidence.**
[Inbox](inbox.md#options-format-contract) owns the brief format;
[Findings](findings.md#emit-shape) owns contextual routing/materialization. The
review template includes their canonical presentation rather than copying it;
[rendered conformance](#agent-output-conformance) checks actual agreement.

### Public Surface for Consumers

[Acceptance](#success-criteria).

`templates` is a public-contract crate. External Rust consumers (e.g. RAG
pipelines, domain-specific review tools) depending on `llm` for typed LLM calls
compose their own templates from `templates`' exposed building blocks:

**Exposed typed context structs:**

- `PinnedContext` (the project-overview + style-rules pinning shape)
- `PreviousFailure`, `VerifierFailure`, `BadWalk`, `DriverNoticeCause` (the
  typed retry-context surface). The per-finding `Finding` record carried inside
  `PreviousFailure::ReviewConcern` is owned by `loom-protocol::gate` (per
  [Findings — Findings and Minting](findings.md#findings-and-minting-1)) and
  re-exported here as a typed dependency.
- `WorkspaceRecovery`, `RecoveryStash`, `WorkspaceAlignment` (the loop-only
  dirty-workspace recovery surface)
- `CriterionStatus`, `EvidenceState`, `CriterionId`, `CriterionAnnotation` (the
  decomposition-phase criterion-evidence surface; consumers writing
  decomposition-style tools reuse this shape against their own caches)
- `PlanContext`, `TodoContext`, `LoopContext`, `ReviewContext` (workflow-phase
  context shapes consumers can either reuse directly or model their own contexts
  after)

**Exposed partial strings:**

Each partial in the _Partials_ table above is also available as a public
`pub const` `&'static str` so consumers can `include!` or `{% include %}` them
in their own templates:

```rust
pub const SCRATCHPAD_PARTIAL: &str = include_str!("templates/partial/scratchpad.md");
pub const CONTEXT_PINNING_PARTIAL: &str = include_str!("templates/partial/context_pinning.md");
// ...
```

**Stability guarantees:**

- Typed context struct field additions are minor version bumps (additive)
- Removing or renaming fields is a major bump
- Partial body changes are minor bumps (consumers don't destructure the body)
- Partial _path_ renames (e.g. `scratchpad.md` → `scratch.md`) are major bumps
  because consumers reference the partial name

**Not exposed:**

- The compiled Askama machinery itself — consumers bring their own template
  engine (Askama, minijinja, raw `format!`, etc.) for their own templates
- Loom's workflow templates (`plan.md`, `todo.md`, `loop.md`, etc.) — consumers
  cannot override these; Loom's workflow shape is opinionated and ships with the
  binary

### Snapshot Test Contract

[Acceptance](#success-criteria).

Every template × representative-input combination has an `insta` snapshot. The
rendered body is the contract shipped to the agent; layout drift slips past
substring assertions. Snapshots surface diffs in PR review. Updates require an
explicit `snapshot updated because: <reason>` line in the PR description (per
the team's testing rules).

## Configuration

[Acceptance](#success-criteria).

Three pinning-related fields on `LoomConfig`, all loaded from
`<workspace>/loom.toml`:

```toml
# Project overview — pinned in every phase
pinned_context = "docs/README.md"

# Style rules — pinned in loop and review
style_rules = "docs/style-rules.md"

# Spec-authoring conventions — pinned in plan
spec_conventions = "docs/spec-conventions.md"
```

All three are project-relative paths. Empty values are rejected at config parse
time as `ConfigError::EmptyPath { field }` — blanking a config does not disable
the pin. To genuinely drop a pin, remove the corresponding `{% include %}` from
the relevant template (a spec change, not a config one). Defaults keep the
bundled documents in front of the agent with zero configuration.

## Success Criteria

### Compaction recovery

- At session start, `.loom/scratch/<key>/` contains `prompt.txt`, `scratch.md`,
  `repin.sh`, and any materialized built-in `skills/` for every agent-bearing
  phase command (plan, todo, loop, gate review, inbox chat)
  [test](open_creates_layout_and_drop_removes_it)

- `<key>` is the joined anchor-label set (or `plan`) for `loom plan`, the work
  epic id for `loom todo`, the bead id for loop/gate worker sessions, and the
  addressed item/filter key for inbox chat
  [test](resolve_scratch_key_uses_plan_anchors_work_epic_or_bead)

- Running `repin.sh` emits a valid `SessionStart[compact]` JSON envelope
  containing banner + `prompt.txt` + `scratch.md` contents
  [test](repin_script_runs_jq_envelope_against_files)

- Running `repin.sh` preserves the full `prompt.txt` bytes in the
  post-compaction envelope before appending `scratch.md`; compacted summaries
  are not accepted as substitutes for the pinned prompt
  [test](repin_script_preserves_full_prompt_verbatim)

- A simulated planning compaction with a fixture `Interview Modes` section
  defining `polish` / `do a polish` as report-only, with no edits applied unless
  explicitly asked, resumes with that definition still present
  [test](compacted_resume_preserves_polish_mode_definition)

- The context-assembly unit canary rejects a vague compacted summary as a
  substitute for the full `polish` report-only mode definition
  [test](post_compaction_polish_canary_requires_full_mode_definition)

- A production-path behavioral canary for a planning session forces or simulates
  compaction, asks `do a polish` after compaction, and fails unless the
  post-compaction answer preserves both the full report-only,
  propose-edits/no-file-edits-unless-asked semantics and a test-only nonce from
  the initial rendered prompt [test](loom_plan_compaction_repin_polish_canary)

- A simulated planning compaction with a fixture `Interview Modes` section
  defining `one by one` as one design question per turn resumes with that
  definition still present
  [test](compacted_resume_preserves_one_by_one_mode_definition)

- Any backend-specific hard-limit fallback preserves instruction, protocol, and
  mode sections verbatim and removes ordinary history before pinned instruction
  text [test](hard_limit_fallback_preserves_pinned_instruction_sections)

- On session end (success or failure), the per-key scratch directory is removed
  [test](close_removes_dir_and_is_idempotent_with_drop)

- Two parallel `loom loop` workers on different beads use independent scratch
  directories and do not collide [test](parallel_keys_get_independent_dirs)

- `partial/scratchpad.md` instructs the agent that the scratchpad is
  agent-lifecycle-only and points at durable destinations for long-term records
  [judge](../tests/judges/loom.sh#test_scratchpad_partial_clarity)

### Engine

<!-- prettier-ignore -->
- All workflow templates compile under Askama with their typed context structs
  [check](cargo build -p loom-templates)

- Each template has a typed context struct with every variable in the template
  body bound as a field [test](template_renders_are_byte_stable_across_runs)

- Templates compile at build time — missing variables are compile errors, not
  runtime errors [test](template_renders_are_byte_stable_across_runs)

<!-- prettier-ignore -->
- Partials are included via Askama's `{% include %}` mechanism [check](grep -q 'partial/context_pinning' crates/loom-templates/templates/loop.md)

- Rendered output is stable across runs for identical inputs, verified by
  `insta` snapshots [test](template_renders_are_byte_stable_across_runs)

<!-- prettier-ignore -->
- Template bodies must not name harness subcommands the spec marks removed
  (`loom run`, `loom check <X>` — see _Removed surface_ in
  [Harness](harness.md)); the rename targets are `loom loop` and
  `loom gate <X>`. Drift breaks every plan / todo / loop / inbox / review
  session by directing the agent at non-existent dispatch (Invariant 3 from
  [Gate](gate.md)) [check](cargo run -p loom-walk -- templates_no_removed_surface)

### Pinning policy

- Planning receives the package contract and can retrieve detailed criteria;
  decomposition receives relevant changed contracts and criteria with historical
  observations distinguished from current coverage and retained blockers, not
  cached passes upgraded to authority. Workers receive applicable acceptance
  obligations explicitly; review receives applicable invariants, criteria, and
  evidence. Uncertain context selection broadens context rather than omitting
  obligations. [test?](quint_phase_context_preserves_applicable_obligations)

- Compaction recovery restores the dispatched contract and resolved acceptance
  context, including the task purpose, criterion identities/text/bindings or
  resolved remediation goals and attribution, without requiring rediscovery or
  reinterpreting references against a different snapshot.
  [test?](quint_context_selection_survives_compaction)

- Worker context construction consumes Loop's resolved acceptance form and
  renders assigned criteria with identity/text/bindings or remediation goals
  with finding attribution, targets, evidence, and processed-batch acceptance.
  It neither reparses references nor invents criterion IDs for broken acceptance
  or treats resolution as a verifier pass.
  [test?](worker_context_consumes_resolved_acceptance)

- Context selection avoids unconditional pinning of every package's full
  acceptance document and model; task-relevant obligations are not optional
  supporting reads.
  [test?](quint_progressive_disclosure_separates_obligations_from_support)

<!-- prettier-ignore -->
- `style_rules.md` partial renders the `style_rules` variable [check](grep -q '{{ style_rules' crates/loom-templates/templates/partial/style_rules.md)

<!-- prettier-ignore -->
- `loop.md` and `review.md` include `style_rules.md`; no other phase template
  does [check](cargo run -p loom-walk -- template_pinning_matrix)

<!-- prettier-ignore -->
- `spec_conventions.md` partial renders the `spec_conventions` variable;
  included only by `plan.md` [check](cargo run -p loom-walk -- template_pinning_matrix)

<!-- prettier-ignore -->
- `todo_success.md` exists, is included only by `todo.md`, and names the
  `LOOM_TODO:` success marker plus the `TodoSuccess` Rust type [check](cargo run -p loom-walk -- template_pinning_matrix)

<!-- prettier-ignore -->
- `todo.md` deliberately omits `progress_markers.md`; generic `LOOM_COMPLETE` /
  `LOOM_NOOP` are wrong-phase success markers for todo [check](cargo run -p loom-walk -- template_pinning_matrix)

- `LoopContext` and `ReviewContext` carry `style_rules: String`; other phase
  contexts do not [test](template_renders_are_byte_stable_across_runs)

- `PlanContext` carries `spec_conventions: String`; other phase contexts do not
  [test](template_renders_are_byte_stable_across_runs)

- `LoomConfig.style_rules` defaults to `"docs/style-rules.md"`;
  `LoomConfig.spec_conventions` defaults to `"docs/spec-conventions.md"`;
  `LoomConfig.pinned_context` defaults to `"docs/README.md"`
  [test](pin_paths_default_to_bundled_docs)

- Empty string values for any pin path are rejected at parse time with
  `ConfigError::EmptyPath { field }` naming the offending field
  [test](empty_pin_path_returns_empty_path_error)

- The `style_rules.md` and `review_rubric.md` partials are rule-family-agnostic:
  their bodies do not enumerate fixed prefixes like `SH-` / `RS-` / `COM-`;
  rule-ID examples in template prose are placeholders, not normative
  [test](review_renders_style_rule_conformance_walkthrough)

<!-- prettier-ignore -->
- Every non-pending cell of the pinning matrix above matches the actual
  `{% include %}` graph in `loom-templates/templates/` (transitive resolution);
  drift in either direction fails the audit [check](cargo run -p loom-walk -- template_pinning_matrix)

<!-- prettier-ignore -->
- The `skill_index.md` partial is included by every agent-bearing template and
  is the only workflow-template location that describes skill discovery/loading
  semantics [check](cargo run -p loom-walk -- template_pinning_matrix)

- `partial/skill_index.md` renders the precomputed `{{ skill_index }}` without
  reconstructing skills-owned disclosure policy, contains no built-in skill body
  literals, and states that skill guidance cannot override phase protocol,
  terminal markers, or gate requirements
  [test](skill_index_partial_renders_precomputed_markdown)

<!-- prettier-ignore -->
- `partial/interview_modes.md` exists, is included by `plan.md` only, and is
  omitted from non-planning templates [check](cargo run -p loom-walk -- template_pinning_matrix)

<!-- prettier-ignore -->
- The interview-modes partial documents `polish` / `do a polish` as a
  report-only review mode that proposes edits but does not apply them unless the
  user explicitly asks [check](bash -c "grep -qi 'report-only' crates/loom-templates/templates/partial/interview_modes.md && grep -qi 'unless explicitly asked' crates/loom-templates/templates/partial/interview_modes.md")

<!-- prettier-ignore -->
- The interview-modes partial documents `one by one` as one question per turn
  with a proposed default and an explicit wait for the user's answer before
  proceeding [check](bash -c "grep -qi 'one question per' crates/loom-templates/templates/partial/interview_modes.md && grep -qi 'wait' crates/loom-templates/templates/partial/interview_modes.md")

- The `chat_marker_final_turn_only.md` partial is included by every
  interactive-session template (`plan`, `inbox`) and documents that inbox may
  use `LOOM_APPLY: {"proposals":[...]}` as its final marker when driver apply is
  requested [test](every_multi_turn_template_includes_chat_marker_partial)

- Worker templates (`todo`, `loop`, `review`) omit the interactive-only
  final-turn partial; nonterminal records may stream before the session's one
  terminal outcome. [test](worker_templates_omit_chat_final_turn_clause)

<!-- prettier-ignore -->
- `partial/chat_interview.md` exists and is included by every
  interactive-session template (`plan`, `inbox`) and by no worker template; the
  body forbids Claude Code's structured option-picker tool for interactive Q&A
  and requires conversational prose instead [check](cargo run -p loom-walk -- template_pinning_matrix)

<!-- prettier-ignore -->
- The partial body names the picker prohibition explicitly so a grep for the
  rule succeeds (no rule-by-implication) [check](grep -qi 'option-picker\|AskUserQuestion' crates/loom-templates/templates/partial/chat_interview.md)

<!-- prettier-ignore -->
- The partial body names the persistence-destination clause distinctively so a
  grep for the rule succeeds: interactive sessions persist cross-session memory
  via the phase-authorized durable surface, not via Claude Code's `MEMORY.md`
  system which is container-local; plan is explicitly barred from bd writes
  while inbox can use bd notes [check](grep -qi 'MEMORY.md\|bd update.*--notes' crates/loom-templates/templates/partial/chat_interview.md)

- `inbox.md` rendered prompt contains the chat-interview discipline clauses
  (picker prohibition + persistence destinations) sourced from the pinned
  partial [test](inbox_template_renders_chat_interview_discipline)

- Inbox proactively explains the problem and decision in plain language,
  compares option changes and practical trade-offs while retaining clarify
  option numbers/titles, and recommends with rationale and its main downside
  before asking for a decision; missing evidence, next investigation, and human
  confirmation remain explicit
  [test](inbox_explains_problem_options_and_recommendation_before_decision)

- With one visible inbox item, chat investigates immediately without asking
  where to start or seeking permission to investigate; writes still require
  human confirmation
  [test](inbox_chat_starts_single_item_investigation_without_asking)

- With multiple visible inbox items, chat asks which item to start with before
  investigating the selected item
  [test](inbox_chat_asks_where_to_start_with_multiple_items)

### Agent-output markers

- Templates that render agent-generated content delimit it with `<agent-output>`
  / `</agent-output>` markers
  [test](agent_output_markers_wrap_each_agent_supplied_field)

### Snapshot tests

- Rendered loop instructions remain distinct ordered-list items, with test
  strategy choices nested under their owning instruction, on both fresh and
  retry dispatches.
  [test](loop_render_preserves_instruction_list_structure_on_fresh_and_retry_dispatch)

- Rendered inbox item and option headings retain their complete dynamic identity
  and Markdown heading level.
  [test](inbox_render_preserves_dynamic_item_and_option_headings)

- Rendered todo sections remain outside preceding dynamic lists; specification
  metadata or criterion rows cannot absorb later context and instructions.
  [test](todo_render_keeps_sections_outside_dynamic_lists)

- The shared Options partial preserves call-site nesting: formatting it as
  standalone Markdown cannot move its rules out of the owning self-report
  marker's list item.
  [test](shared_options_partial_preserves_call_site_list_nesting)

- Every template × representative-input combination has an `insta` snapshot
  [test](every_askama_template_has_snapshot)

<!-- prettier-ignore -->
- Snapshot tests run under the workspace clippy test exemptions (no per-file
  `#![allow(clippy::unwrap_used, ...)]`) [check](cargo run -p loom-walk -- loom_templates_snapshots_no_crate_root_allow)

### Sibling-spec editing

- `partial/sibling_spec_editing.md` documents that creating a new sibling spec
  is a valid planning-session outcome, requires an index row, and says plan does
  not allocate a bead/epic
  [judge](../tests/judges/loom.sh#judge_sibling_spec_editing_documents_split)

### Pinning matrix walker pending support

- The pinning-matrix walker accepts `?` (pending addition) and `~` (pending
  removal) as valid cell values in the matrix alongside `✓` and blank, per
  [Verify — Pending support in structured walker input](verify.md#pending-support-in-structured-walker-input)
  [test](template_pinning_matrix_accepts_pending_cells)

- `?` + template-doesn't-include → silent pass (pending); `?` +
  template-includes → walker fails with `pending-marker-resolved` so the author
  drops `?` to `✓` in the same diff
  [test](pending_addition_marker_fires_when_template_now_includes)

- `~` + template-includes → silent pass (pending); `~` +
  template-doesn't-include → walker fails with `pending-marker-resolved` so the
  author drops `~` to blank in the same diff
  [test](pending_removal_marker_fires_when_template_no_longer_includes)

<!-- prettier-ignore -->
- The walker's existing per-cell assertion is unchanged for non-pending cells:
  `✓` requires transitive include; blank forbids transitive include; mismatch
  fails the walker [check](cargo run -p loom-walk -- template_pinning_matrix)

### Planning-rubric pending discipline

- The rendered planning rubric directs the agent to map contract sections and
  behavioral table rows to the owning package's `tests.md` criteria through
  ordinary Markdown section links, preserves exactly one binding per criterion
  and the pending policy, and does not demand duplicated annotations in
  `spec.md`, a mapping registry, or equal row/annotation counts.
  [test?](plan_rubric_maps_contracts_to_package_acceptance)

<!-- prettier-ignore -->
- `partial/plan_stage_rubric.md` exists and is included by `plan.md` only
  [check](cargo run -p loom-walk -- template_pinning_matrix)

- The partial body distinguishes **binary-pending** from **assertion-pending**
  pending-modifier cases with worked examples, so a planning agent author
  understands both shapes warrant `?`
  [test](plan_stage_rubric_distinguishes_binary_from_assertion_pending_by_exit_status)

<!-- prettier-ignore -->
- The partial body names the **"added and modified" rule** explicitly — pending
  discipline applies to annotations the session adds AND to annotations whose
  target the session changed in a way that breaks resolution [check](grep -qi 'added.*modified\|added and modified\|modified.*annotation' crates/loom-templates/templates/partial/plan_stage_rubric.md)

<!-- prettier-ignore -->
- The partial body names the **structured walker input** rule — planning edits
  to matrix / surface / wire-format input use the walker's `?` (pending
  addition) and `~` (pending removal) cell syntax for pending elements, not the
  SC-level `?` modifier, per Verify — Pending support in structured walker input
  [check](grep -qi 'structured.*input\|pending.*cell\|walker.*input' crates/loom-templates/templates/partial/plan_stage_rubric.md)

<!-- prettier-ignore -->
- The partial body names the **self-cleaning obligation** — the `?` must be
  dropped in the same diff that resolves the target, else
  `UnneededPendingMarker` fires [check](grep -qi 'UnneededPendingMarker\|self-cleaning\|drop the.*marker' crates/loom-templates/templates/partial/plan_stage_rubric.md)

### Workspace recovery

- `LoopContext` carries `workspace_recovery: Option<WorkspaceRecovery>`
  separately from `previous_failure`; rendering it does not require or increment
  the retry `attempt` counter
  [test](loop_context_renders_workspace_recovery_without_retry_attempt)

- `WorkspaceRecovery` carries pre-stash status, stable stash commit, stash
  selector/message, target integration tip, and alignment state (`Clean`,
  `Rebased`, or `Conflict { files }`)
  [test](workspace_recovery_context_is_publicly_constructible_from_crate_root)

<!-- prettier-ignore -->
- `partial/workspace_recovery.md` tells the worker to inspect the stash with
  `git stash show --stat` and `git stash show -p`, then intentionally
  apply/cherry-pick, leave, or drop it; conflict alignment is framed as
  agent-owned merge-conflict recovery with `LOOM_CLARIFY` as the human-decision
  fallback [check](bash -c "grep -qi 'git stash show --stat' crates/loom-templates/templates/partial/workspace_recovery.md && grep -qi 'LOOM_CLARIFY' crates/loom-templates/templates/partial/workspace_recovery.md")

- When both `previous_failure` and `workspace_recovery` are present, `loop.md`
  renders `previous_failure` first and `workspace_recovery` second so the worker
  sees why the prior attempt failed before inspecting preserved dirty work
  [test](loop_template_renders_previous_failure_before_workspace_recovery)

- When `workspace_recovery` is present, the worker's final prose summary is
  prompted to mention how the stash was handled, but the driver does not parse
  that prose or fail `LOOM_COMPLETE` solely because the stash remains
  [test](workspace_recovery_summary_prompt_is_non_authoritative)

### Todo success shape

- The rendered todo prompt asks for task-to-criterion assignment proposals using
  supplied criterion IDs in the shared typed handoff, delegates binding metadata
  persistence to Rust, and does not instruct the agent to compute IDs or issue
  binding-metadata writes through `bd`.
  [test?](todo_prompt_proposes_bindings_for_driver_persistence)

<!-- prettier-ignore -->
- `partial/todo_success.md` is the canonical presentation of Todo's success
  payload under Protocol's message contract and names the
  `loom-protocol::todo::TodoSuccess` type [check](grep -q 'LOOM_TODO:' crates/loom-templates/templates/partial/todo_success.md)

<!-- prettier-ignore -->
- `todo.md` includes `todo_success.md` via `{% include %}` rather than restating
  the success marker contract inline [check](grep -q 'partial/todo_success.md' crates/loom-templates/templates/todo.md)

<!-- prettier-ignore -->
- `partial/progress_markers.md` contains no `LOOM_TODO:` literal; todo success
  belongs to `todo_success.md` [check](bash -c "! grep -nE 'LOOM_TODO:' crates/loom-templates/templates/partial/progress_markers.md")

- Rendered `todo.md` prompts instruct the agent that `LOOM_COMPLETE` and
  `LOOM_NOOP` are wrong-phase success markers for todo
  [test](todo_template_rejects_generic_success_markers)

### Review emit shape

- Rendered review guidance uses the canonical message/domain metadata, requires
  strict JSON with escaped evidence, preserves explicit finding/concern pairing,
  and does not authorize reviewer Beads mutation or duplicate evidence
  narration.
  [test?](review_prompt_uses_strict_shared_messages_and_domain_pairing)

<!-- prettier-ignore -->
- `review.md` includes `findings_walk.md` via `{% include %}` rather than
  restating the wire format [check](grep -q 'partial/findings_walk.md' crates/loom-templates/templates/review.md)

<!-- prettier-ignore -->
- `review.md` does not contain a `bd create` invocation (the reviewing agent
  reports findings without mutating Beads; trusted driver materialization is
  separate) [check](bash -c "! grep -nE 'bd create|bd mol bond|bd update --add-label' crates/loom-templates/templates/review.md")

<!-- prettier-ignore -->
- `partial/progress_markers.md` covers the progress markers (`LOOM_COMPLETE`,
  loop-only `LOOM_NOOP`) and contains no `LOOM_CONCERN:` or `LOOM_FINDING:`
  literal — those belong to `findings_walk.md` per the partial split documented
  in [Findings — Findings and Minting](findings.md#findings-and-minting-1)
  [check](bash -c "! grep -nE 'LOOM_CONCERN:|LOOM_FINDING:' crates/loom-templates/templates/partial/progress_markers.md")

- Rendered progress-marker guidance distinguishes review `LOOM_COMPLETE` (clean
  inspection, no diff expected) from loop `LOOM_COMPLETE` (closed bead with
  non-empty diff) and keeps `LOOM_NOOP` loop-only
  [test](progress_markers_render_phase_specific_diff_rules)

- Loop and Todo waiting guidance requires actual prerequisites, non-closure of
  waiting work, preservation, and driver-attributed visible parking/resumption;
  it distinguishes a decision record from the session's waiting terminal.
  [test?](loop_and_todo_render_attributed_waits_and_nonterminal_decisions)

<!-- prettier-ignore -->
- `partial/self_report_markers.md` covers direct loop/todo self-report markers
  (`LOOM_RETRY`, `LOOM_CLARIFY`, `LOOM_BLOCKED`) and contains no `LOOM_CONCERN:`
  or `LOOM_FINDING:` literal [check](bash -c "! grep -nE 'LOOM_CONCERN:|LOOM_FINDING:' crates/loom-templates/templates/partial/self_report_markers.md")

<!-- prettier-ignore -->
- `partial/review_self_report_markers.md` covers review-only cannot-complete
  guidance, forbids bd mutation, and contains no `LOOM_CONCERN:` or
  `LOOM_FINDING:` literal [check](bash -c "! grep -nE 'LOOM_CONCERN:|LOOM_FINDING:' crates/loom-templates/templates/partial/review_self_report_markers.md")

- Rendered `review.md` prompts include review-specific self-report guidance that
  forbids bd mutation, omits direct bd-backed `LOOM_CLARIFY` persistence
  instructions, and routes clarify-worthy decisions through `route=\"clarify\"`
  finding evidence or `LOOM_BLOCKED` when no options can be articulated
  [test](review_self_report_markers_do_not_authorize_bd_writes)

<!-- prettier-ignore -->
- Interactive-session templates (`plan.md`, `inbox.md`) deliberately **omit**
  direct and review self-report partials because the worker-phase cannot-finish
  markers are not valid emit options for interactive sessions — the human
  resolves friction in-turn. Including either partial would teach interactive
  agents about markers they cannot emit [check](cargo run -p loom-walk -- template_pinning_matrix)

<!-- prettier-ignore -->
- The partial body names `LOOM_RETRY` semantics distinctively (transient /
  environmental / agent-self-reset, consumes a `[loop] max_retries` slot,
  escalates to `loom:blocked` cause `retry-exhausted` on exhaustion) so a grep
  for the rule succeeds [check](grep -qi 'LOOM_RETRY' crates/loom-templates/templates/partial/self_report_markers.md)

<!-- prettier-ignore -->
- The partial body distinguishes `LOOM_BLOCKED` from `LOOM_CLARIFY`: blocked =
  genuine dead end, no candidate resolutions, with a reason explaining why
  options cannot be enumerated; clarify = decision the agent can frame as a
  structured `## Options — …` block. The discriminator (can the agent enumerate
  options?) is named explicitly [check](grep -qi 'candidate resolution\|enumerate options' crates/loom-templates/templates/partial/self_report_markers.md)

<!-- prettier-ignore -->
- The direct partial body identifies the direct worker scope: `LOOM_CLARIFY`
  persistence applies to `loop` and `todo`, while review uses review-specific
  finding evidence; interactive sessions (`plan`, `inbox`) do not emit worker
  self-report markers because the human resolves friction in-turn [check](grep -qi 'loop.*todo\|review-specific\|interactive.*session' crates/loom-templates/templates/partial/self_report_markers.md)

<!-- prettier-ignore -->
- The review self-report partial names the review discriminator: retry for
  fresh-dispatch environmental failures, clarify-worthy decisions as
  `route="clarify"` findings, and dead ends with no options as `LOOM_BLOCKED`
  [check](grep -qi 'route="clarify"\|Review is inspection-only\|LOOM_BLOCKED' crates/loom-templates/templates/partial/review_self_report_markers.md)

### Typed `PreviousFailure`

- Recovery rendering surfaces the decoded retry reason and distinguishes
  reporting dedicated decisions, genuinely waiting, and terminal semantic
  blocking, without teaching a bare reason or terminal clarification.
  [test?](agent_retry_display_teaches_shared_decision_and_terminal_roles)

- `FindingParseError` is defined in `loom-protocol::gate` and re-exported from
  `loom-templates::finding` / `loom-workflow::review::finding` as the per-record
  wire-format error consumed by `BadWalk::MalformedFinding.errors`
  [test](loom_templates_re_exports_finding_contract_from_loom_protocol)

- The `Display for PreviousFailure` rendering of `BadWalk(Concern)` appends a
  per-finding digest of `parsed_findings` when non-empty (the agent's diagnosis
  from the streamed findings is surfaced even when the terminal was malformed)
  [test](bad_walk_concern_display_renders_parsed_findings_digest_when_present)

- The `Display for PreviousFailure` rendering of
  `BadWalk(FindingsWithoutConcern)` appends a per-finding digest of `findings`
  so the agent's next iteration sees the diagnosis it just emitted
  [test](bad_walk_findings_without_concern_display_renders_findings_digest)

- Mixed malformed-finding recovery rendering exposes record errors, retained
  valid findings, and decoded/malformed/missing terminal context within the
  existing budget. Presentation truncation does not discard the underlying typed
  diagnosis or replace it with an empty finding list.
  [test?](mixed_bad_walk_display_surfaces_findings_errors_and_terminal)

- `VerifierFailure` carries `target: String`, `exit_code: i32`,
  `stderr_tail: String` (capped per-block at ~1500 chars)
  [test](verifier_failure_stderr_tail_capped_per_block)

- Total `previous_failure` budget capped at `PREVIOUS_FAILURE_MAX_LEN = 4000`
  chars; multi-block variants split budget across entries with later entries
  truncated first [test](verify_failures_split_budget_truncates_later_first)

- Direct template callers may supply the compatibility `review_notes` field
  alongside `previous_failure`; its content still renders under `Review notes:`.
  The workflow does not synthesize legacy review flags.
  [test](run_template_renders_review_notes_block_when_set)

- Each `PreviousFailure` variant renders with its documented framing prefix
  (`DriverNotice` → "Previous attempt:", `VerifyFailures` → "Verifier failures
  from previous attempt:", `ReviewConcern` → "Review raised a concern ({label}):
  {summary}" with `{label}` derived from streamed finding tokens, `BadWalk` →
  per-variant fragment naming the specific malformation, `BuildFailure` → "Build
  failed at ...:", `TreeNotClean` → "Working tree was not clean after the bead
  committed:", `PostIntegrateFail` → "After rebasing onto the integration
  branch, the post-integration verify failed.", and `IntegrationConflict` →
  "Your bead branch could not be rebased onto the integration branch")
  [test](previous_failure_variant_framings_match_spec)

- `TreeNotClean` renders the dirty-path list one-per-line and appends a
  `"+N more"` suffix line when the upstream driver truncated past 30 entries
  [test](tree_not_clean_renders_path_list_with_truncation_suffix)

### Attempt counter

- `loop.md` omits the attempt line when `attempt == 0`
  [test](run_template_omits_attempt_line_when_zero)

- `loop.md` renders "Retry attempt {N} — previous attempt failed with: …" when
  `attempt > 0 && previous_failure.is_some()`
  [test](run_template_renders_attempt_line_on_retry)

### First-instruction reframe

- `loop.md` prepends "Re-read the previous failure block above and address its
  specific concern before re-implementing." when
  `attempt > 0 && previous_failure.is_some()`
  [test](run_template_prepends_first_instruction_reframe_on_retry)

- Reframe is omitted when `previous_failure.is_none()`
  [test](run_template_omits_first_instruction_reframe_on_fresh_dispatch)

- Reframe is omitted when `attempt == 0`, even if a `previous_failure` value is
  present [test](run_template_omits_first_instruction_reframe_when_attempt_zero)

<!-- prettier-ignore -->
- Reframe wording is generic (one form regardless of variant); per-variant
  detail lives inside the previous-failure block itself [check](grep -q 'Re-read the previous failure block above' crates/loom-templates/templates/loop.md)

### Loop completion self-check and self-review

- `loop.md` instructs the worker to run
  `loom gate verify --diff <bead-base>..HEAD` (or `@{u}..HEAD` only when
  upstream is that base) before emitting `LOOM_COMPLETE`; it does not name
  `loom gate verify --diff HEAD` as the final self-check
  [test](run_template_uses_injected_self_check_range_not_head_shorthand)

- `loop.md` tells the worker to rerun the self-check after any later commit or
  hook-generated file change
  [test](run_template_requires_self_check_rerun_after_post_check_changes)

- `loop.md` requires prompt-level self-review before the final outcome: re-read
  criteria, inspect the final diff, check style/spec fit, and fix issues or use
  the appropriate Protocol message and Loop outcome instead of false completion.
  Decision reporting is independent of terminality.
  [judge](../tests/judges/loom.sh#judge_loop_self_review_before_complete)

### Public surface

<!-- prettier-ignore -->
- `templates` exposes `PreviousFailure`, `VerifierFailure`, `BadWalk`,
  `DriverNoticeCause`, `WorkspaceRecovery`, `RecoveryStash`,
  `WorkspaceAlignment`, `CriterionStatus`, `EvidenceState`, `CriterionId`,
  `CriterionAnnotation`, `LoopContext`, `ReviewContext`, `PlanContext`,
  `TodoContext`, and `PinnedContext` as public types consumable from external
  crates [check](cargo run -p loom-walk -- loom_templates_public_types)

<!-- prettier-ignore -->
- Each partial in the _Partials_ table is also exposed as a public
  `&'static str` constant (e.g. `SCRATCHPAD_PARTIAL`, `CONTEXT_PINNING_PARTIAL`,
  etc.) for consumer template composition [check](cargo run -p loom-walk -- loom_templates_public_partial_constants)

<!-- prettier-ignore -->
- Loom's workflow template bodies themselves (`plan.md`, `todo.md`, `loop.md`,
  `review.md`, `inbox.md`) are NOT publicly exported — only the typed contexts
  and partial strings [check](cargo run -p loom-walk -- loom_templates_workflow_templates_not_exported)

### Criterion-status surface

<!-- prettier-ignore -->
- `TodoContext` carries `criterion_status: Vec<CriterionStatus>`; no other phase
  context does [check](cargo run -p loom-walk -- todo_contexts_carry_criterion_status)

- Inline `todo.md` rendering surfaces every changed spec's `CriterionStatus` rows
  with criterion text, annotation, and evidence state so the agent can
  distinguish recorded results from stale annotations or missing observations
  [test](todo_template_renders_typed_criterion_status_rows)

- Dossier-mode Todo rendering keeps the complete roster and explicit evidence
  index without inlining large diffs or notes, and supplies durable-draft
  resumption guidance without weakening the final success protocol.
  [test](todo_dossier_render_keeps_roster_and_resumption_without_inlining_evidence)

### Decomposition discipline

<!-- prettier-ignore -->
- `partial/decomposition_discipline.md` exists and is included by `todo.md`
  only; the body names the exact changed-spec roster, the evidence-confirmation
  obligation, and the `LOOM_TODO` success shape [check](cargo run -p loom-walk -- template_pinning_matrix)

<!-- prettier-ignore -->
- The partial body names the discipline distinctively (so a grep catches
  accidental emptying) [check](grep -qi 'exact.*roster\|evidence-confirmed\|LOOM_TODO' crates/loom-templates/templates/partial/decomposition_discipline.md)

- Rendered `todo.md` prompts contain a clause committing the agent to confirm
  missing work by inspection before authoring any non-audit bead
  [test](todo_template_renders_pre_decomposition_audit_clause)

- Rendered Todo guidance puts separate decision briefs on dedicated children,
  reports the full discovered set, and waits on the work epic only when
  decomposition itself has actual prerequisites; decision children cannot
  satisfy implementation assignments or the success roster.
  [test?](todo_prompt_separates_decisions_waiting_and_implementation_handoff)

<!-- prettier-ignore -->
- `todo.md` receives an already-created work epic from the driver before any
  path that can emit `LOOM_CLARIFY` [check](cargo run -p loom-walk -- todo_template_uses_driver_created_work_epic)

### Agent-output conformance

- Executable examples extracted from actual rendered phase prompts decode and
  admit through the common production protocol, covering every phase-permitted
  emit variant without teaching another phase's executable output shapes.
  [test?](rendered_phase_message_examples_decode_under_shared_contract)

- Rendered finding examples cover the complete typed prompt-visible token,
  target, and scope subset and resolve through the real workspace validator
  against fixture specs, files, and anchors; wrong combinations fail.
  [test?](rendered_finding_examples_resolve_prompt_visible_token_target_scope_subset)

- Conformance retains independent literal positive/negative fixtures and
  boundary cases; deliberate spelling, arity, role, target, scope, and rendered
  example drift fails even when generated values still round-trip together.
  [test?](prompt_parser_conformance_keeps_independent_negative_wire_fixtures)

- Closed protocol facts and prompt-visible domain metadata come from the
  canonical typed contracts, not an independent production token/phase registry.
  Rendering and exhaustive coverage tests expose missing or inconsistent
  projections. [test?](protocol_metadata_controls_prompt_variant_coverage)

### Unit tests

- Template rendering tests cover every Askama template with representative
  inputs [test](template_renders_are_byte_stable_across_runs)

### Snapshot testing

- Every Askama workflow template has a representative `insta` snapshot
  [test](every_askama_template_has_snapshot)

### Chat Discipline

- Inbox presents the problem, competing options, trade-offs, and a reasoned
  recommendation before asking for a decision.
  [test](inbox_explains_problem_options_and_recommendation_before_decision)

## Requirements

### Consumer composition

1. **Compiled templates with consumer-composable typed building blocks** —
   Askama engine, per-phase templates, partials, and per-phase pinning policy
   live in [Templates](#prompt-composition). The crate that builds them
   (`loom-templates`) is part of
   [Harness's crate layout](harness.md#crate-layout). `loom-templates` is
   **public-contract**: it exposes its typed context structs (`PinnedContext`,
   `PreviousFailure`, `LoopContext`, etc.) and partial-string constants so
   external Rust consumers can compose their own templates from the same
   building blocks Loom's workflow uses. Loom's workflow templates themselves
   remain compile-time Askama and internal — consumers do not override them.

### Functional

1. **Compiled workflow templates.** Every Loom-workflow phase prompt (`plan`,
   `todo`, `loop`, `review`, `inbox`) is an Askama template compiled into the
   binary. Template correctness is verified at compile time. No per-project
   mechanism hot-overrides Loom's workflow templates at runtime;
   `loom tune phase fast|run|full` and `loom tune partial fast|run|full` create
   reviewed source-change proposals instead. (Consumers writing their own
   templates for their own LLM calls via `llm` use the public typed building
   blocks described below; this FR is specifically about Loom's own workflow
   templates.)
2. **One template per phase** as enumerated in _Template Files_ above; `plan`
   and `todo` no longer split into new/update modes.
3. **Partials** as enumerated in _Partials_ above. Each partial declares which
   templates include it; the matrix in _Pinning Policy_ is the authoritative
   listing.
4. **Typed context per template.** Each template has a Rust
   `#[derive(Template)]` struct with one field per variable. The variable set is
   enumerated in _Template Variables_.
5. **Per-phase pinning.** Partial inclusion follows _Pinning Policy_;
   `style_rules.md` is pinned in `loop` and `review` only; `spec_conventions.md`
   is pinned in `plan` only; `todo_success.md` is pinned in `todo` only. Matrix
   cells use the four-value vocabulary `✓` / blank / `?` (pending addition) /
   `~` (pending removal) per
   [Verify — Pending support in structured walker input](verify.md#pending-support-in-structured-walker-input);
   the pinning-matrix walker enforces the assertion at the appropriate scope and
   fails with `pending-marker-resolved` when a pending marker's state catches
   up.
6. **Rule-family agnosticism.** The `style_rules.md` and `review_rubric.md`
   partial bodies discover rule families from the pinned `{{ style_rules }}`
   document. Template bodies do not enumerate fixed prefixes.
7. **Agent-output markers.** All agent-generated content rendered back into a
   prompt is wrapped in `<agent-output>` / `</agent-output>`.
8. **Skill index.** `partial/skill_index.md` is included by every agent-bearing
   template and renders the prompt-ready `SkillIndexMarkdown` produced under
   [Skills](skills.md#registration-and-progressive-disclosure) without
   reinterpreting it.
9. **Template tuning boundary.** Template source remains compiled and cannot be
   hot-loaded; tuning proposal isolation, validation, and inbox exposure are
   owned by [Tuning](tuning.md#tune-proposal-worktrees-and-beads).
10. **Snapshot tests.** Every template × representative-input combination has an
    `insta` snapshot.

### Context and public API

1. **First-instruction reframe.** When
   `attempt > 0 && previous_failure.is_some()`, `loop.md` prepends "Re-read the
   previous failure block above and address its specific concern before
   re-implementing." Single generic form — per-variant detail lives in the
   previous-failure block itself.
2. **Public surface for consumers.** `templates` is a public-contract crate.
   Exposed: `PreviousFailure` (and its sub-types), `WorkspaceRecovery` (and its
   sub-types), `CriterionStatus`, `EvidenceState`, `CriterionId`,
   `CriterionAnnotation`, `SkillIndexMarkdown`, `PlanContext`, `TodoContext`,
   `LoopContext`, `ReviewContext`, `PinnedContext`, and the partial-string
   constants for each entry in the _Partials_ table. Loom's workflow template
   bodies themselves are not exposed — consumers compose their own templates
   from the typed contexts + partial strings, not from Loom's workflow
   templates. Stability: additive type changes are minor bumps; removing or
   renaming fields / partial paths is a major bump.

   **Dependency on `loom-protocol`.** The common output enum, decoder, and phase
   projections live in `loom-protocol::output` under [Protocol](protocol.md).
   Templates consumes those typed facts, without importing runtime
   orchestration. The typed gate wire-format contract (`Finding`,
   `ConcernToken`, `FindingTarget`, `BadWalk`, `WalkOutput`, etc.) lives in
   `loom-protocol::gate` — see
   [Findings — Canonical contract location](findings.md#canonical-contract-location-1).
   The typed todo success contract (`TodoSuccess`, `TodoSpecSuccess`,
   `TodoSpecOutcome`, `TodoFingerprint`) lives in `loom-protocol::todo` per
   [Harness](harness.md). `loom-templates` depends on `loom-protocol` so
   `PreviousFailure::ReviewConcern { findings: Vec<Finding> }` and
   `PreviousFailure::BadWalk(BadWalk)` can carry the typed values;
   `loom-templates` re-exports the gate contract via `pub use` so existing
   consumers importing from `loom-templates::finding` continue to compile. The
   intended consumption shape for a consumer writing their own LLM pipeline
   against loom: depend on `loom-protocol` (parse `loom gate ...` subprocess
   stdout into typed `WalkOutput`), depend on `loom-templates` (compose their
   own Askama template body that `{% include %}`s `PARTIAL_*` constants and
   fills typed contexts), depend on `loom-llm` (run the conversation loop). The
   three crates compose; loom CLI is itself one such consumer.

   **Dogfood is structural.** Loom CLI uses the same Askama mechanism, the same
   exposed partials, and the same typed contexts a consumer would use — there is
   no "loom's special path" vs "consumer's path." Loom's CLI binary depends on
   `loom-templates` exactly like a consumer would. The boundary that keeps
   consumers from forking loom's workflow bodies is the deliberate non-exposure
   of those bodies (the "Loom's workflow template bodies themselves are not
   exposed" rule in the public surface requirement), not a divergent loading
   mechanism.

   `PARTIAL_FINDINGS_WALK` is the canonical review presentation of Protocol's
   message encoding and Findings' payload/pairing contract. Consumers compose it
   with the public finding adapter over the shared decoder. The
   `template_wire_format_restatement` walk prevents copied wire prose; it does
   not prove prompt/parser agreement. That evidence comes from
   [rendered conformance](#agent-output-conformance), including contextual
   resolution and independent negative fixtures. Pinning matching releases
   avoids release skew but does not validate a consumer's custom composition.

3. **Chat discipline in interactive sessions.** `partial/chat_interview.md`,
   pinned in every interactive-session template (`plan`, `inbox`), requires the
   interactive agent to conduct conversations as back-and-forth prose and
   forbids Claude Code's structured option-picker tool (`AskUserQuestion` or any
   equivalent multi-choice widget). Options are listed inline in prose; the user
   replies in prose. The planning-only `partial/interview_modes.md` defines
   shorthand modes on top of that chat discipline: `polish` is report-only
   spec/doc review that does not apply edits unless explicitly asked, and
   `one by one` is one design question per turn with an explicit wait for the
   user's answer before moving on. The chat-interview partial also carries the
   **persistence-destination clause**: session-bridging memory (decisions,
   context, follow-ups) goes only to the durable surface the phase authorizes:
   `loom plan` writes spec/index markdown or implementation notes and does not
   write bd, while `loom inbox` can use bd notes/descriptions for resolutions.
   Claude Code's `MEMORY.md` system is container-local and disappears with the
   container. The "one by one" sub-mode is planning-specific and lives in a
   separate partial; the chat-discipline rules above apply to every interactive
   session, including inbox-chat.
4. **Criterion-status surface for decomposition.** `TodoContext` carries
   `criterion_status: Vec<CriterionStatus>` where each row exposes `spec_label`,
   typed `criterion_id`, criterion text, typed annotation, and `EvidenceState`
   (`Current`, `Missing`, `StaleAnnotation`). The driver populates the surface
   by parsing the changed specs and joining against `.loom/cache.db`'s criterion
   evidence cache. Missing cache rows become `EvidenceState::Missing`, never no
   work. The partial may present observation-age hints for inspection, but those
   hints do not decide current coverage. It renders the supplied
   [Evidence-owned coverage projection](evidence.md#coverage-projection),
   including freshness requirements, blockers, and unavailable admission;
   neither elapsed age nor a historical `Current` pass establishes
   admissibility. `TodoContext` separates inline display from dossier-backed
   disclosure through `EvidenceDelivery`; this is presentation, not a second
   workflow or a coverage decision. Production uses
   [Todo's bounded evidence delivery](todo.md#bounded-evidence-delivery-and-resumption),
   retaining the roster and manifest in the entry prompt while complete typed
   rows, notes, and diffs are available through the referenced pages. Inline
   rendering remains available for focused composition and rendering tests; both
   use the same criterion-row presentation.

### Recovery context

1. **Workspace recovery in `loop`.** `partial/workspace_recovery.md`, pinned in
   `loop` only, renders the driver-created recovery stash context for dirty bead
   workspaces. It is separate from `PreviousFailure` and retry `attempt`, but
   may render in the same loop prompt after `PreviousFailure`: it tells the
   worker which stash commit/message preserves prior dirty work, the pre-stash
   git status, the target integration tip, and whether branch alignment is
   clean, rebased, or conflicted. The worker inspects the stash before normal
   work, handles or deliberately leaves it, mentions that choice in final prose,
   and uses `LOOM_CLARIFY` when recovery needs a human decision. `LOOM_COMPLETE`
   remains payload-free; the driver does not parse stash-handling prose or fail
   solely because the stash remains.

### Non-Functional

1. **Compile-time validation.** Template syntax errors, undefined variables, and
   missing partial files all fail the build, not discovered at runtime.
2. **Style.** Follows the team's
   [`docs/style-rules.md`](../docs/style-rules.md).

#### loom-templates

[Acceptance](#success-criteria).

- All templates compile — Askama enforces this at build time; an explicit
  `cargo nextest run -p loom-templates` is the regression gate
- Template rendering with representative inputs produces output containing
  required partials, agent-output wrapping, and applied truncation (see
  [Tests — Template render contract](tests.md#template-render-contract))
- Layout regressions caught by `insta` snapshots (see
  [Snapshot Test Contract](#snapshot-test-contract))
- Partial inclusion works (context pinning, exit signals, spec header,
  companions, implementation notes)

## Out of Scope

- **Spec-lifecycle CLI commands.** Splitting, merging, renaming, and superseding
  specs are decisions made inside a planning session, with judgment applied to
  which sections move, which beads reassign, and which cross-refs rewrite. The
  CLI exposes no dedicated split / merge / rename / supersede commands.
- **Runtime override of Loom's workflow templates.** Loom's `plan` / `todo` /
  `loop` / `review` / `inbox` templates are Askama, compiled into the binary.
  `loom tune phase fast|run|full` / `loom tune partial fast|run|full` may
  propose source edits in an isolated worktree, but there is no per-project
  template-fetch or runtime template override for Loom's own templates.
  Project-specific prompt tweaks to Loom's workflow happen via `pinned_context`,
  `style_rules`, `spec_conventions`, skills, and per-spec implementation notes.
  Consumers writing their _own_ templates (for their own LLM calls via `llm`)
  compose them from the exposed typed building blocks (above) — that path is
  supported and is _not_ what this exclusion covers.
- **Runtime template engine for consumer overrides of Loom's workflow
  templates.** Adding a runtime engine (e.g. `minijinja`) to allow consumers to
  drop in replacements for Loom's compiled Askama templates is bolt-on-able
  after the typed-context public surface lands and is deferred until a concrete
  consumer asks.
- **Untyped `previous_failure`.** `LoopContext.previous_failure` is
  `Option<PreviousFailure>` — a typed enum, not a free string. Free-string
  detail (driver formats prose into a String the template prints unchanged) is
  excluded so heading shape, caps, and multi-cause composition stay owned by the
  typed contract rather than re-derived at every emit site.
- **Template content changes.** The _rules_ themselves live in
  `docs/style-rules.md`; this spec only pins the file and does not own its
  content. The _conventions_ themselves live in `docs/spec-conventions.md`
  similarly.
- **Selective rule filtering in the pin.** The `partial/style_rules.md` pin
  points at the whole document; agents read the families relevant to their work.
  Revisit if prompt-size measurements show the unselected pin is materially
  expensive.

# Loom Templates

Askama template engine, partials inventory, per-phase pinning
policy, snapshot-test contract, and public-contract typed building
blocks consumers compose into their own templates.

## Problem Statement

Loom's agent-bearing workflow phase prompts (`plan`, `todo`, `loop`,
`review`, `inbox`) are rendered from Askama templates compiled into
the binary. `loom gate verify` is deterministic and renders no
template. The template surface is its own concern: which partials
exist, which template renders which partial in which phase, which
context struct each template binds to, and what the snapshot gate
looks like.

`templates` is a **public-contract crate**: external Rust
consumers depending on `llm` for typed LLM calls can compose
their own templates using `templates`' exposed typed context
structs (`PinnedContext`, `PreviousFailure`, `LoopContext`, etc.) and
partial strings. Loom's own *workflow templates* remain compiled-in
Askama and internal — consumers do not override them — but the
building blocks that go into those templates are shared.

[harness.md](harness.md) owns the crate that builds these
templates and the runtime that consumes rendered prompts; this spec
owns the prompt surface itself.

## Architecture

### Template Files

One template per agent-bearing phase:

- `plan.md`
- `todo.md`
- `loop.md`, `review.md`, `inbox.md`

`loom plan [SPEC_LABEL ...]` uses one planning template. The optional
labels are initial context anchors; new-vs-update is inferred from the
spec/index files the interview edits, not from separate template modes.

`loom todo` uses one decomposition template. The driver performs
changed-spec preflight, creates or reuses the `loom:todo` work epic,
and injects the exact changed-spec roster before the template renders;
there is no `todo_new` / `todo_update` split.

`loom gate verify` is deterministic — it runs project hooks,
verifiers, audits, and linters without rendering any agent prompt — so
it has no template. `loom gate review` is the LLM-judged counterpart
and has its own template, distinct from `loop.md` because the review
session has different inputs (diff, molecule/bead context, sibling
diffs, typed deterministic gate evidence) and a rubric-walk objective
rather than an implement-the-bead objective.

Each template has a matching `#[derive(Template)]` context struct in the
same crate. The Askama build verifies every variable referenced in the
template body has a matching field on its context struct — missing
variables are compile errors, unused fields trigger the `unused`
workspace lint.

### Template Tuning Proposals

Loom's workflow templates remain compiled source. `loom tune phase fast|run|full`
and `loom tune partial fast|run|full` may propose source edits, but they do so in
an isolated `.loom/tune/<bead-id>/repo/` worktree and never as runtime template
overrides. A candidate template proposal must pass the same compile/render
boundary real source uses before it reaches human review:

1. Askama compiles the candidate templates against their typed contexts.
2. Representative render snapshots are produced in the proposal worktree.
3. Template conformance walkers validate the include graph, terminal-marker
   ownership, options/findings wire-format single-source rules, and surface
   references.
4. The proposal is exposed through `loom inbox` only after validation succeeds.

This lets the SkillOpt discipline improve templates while preserving the core
safety property: phase protocol is reviewed source, not dynamic prompt state.

### Partials

Reusable fragments included via `{% include "partial/<name>.md" %}`.
Current and target v1 set; pending additions are marked in the pinning matrix:

| Partial | Purpose |
|---------|---------|
| `context_pinning.md` | Pin the project-overview file (`pinned_context`) |
| `style_rules.md` | Pin the style-rules file (`style_rules`) — see *Style-Rules Partial* below |
| `spec_conventions.md` | Pin the spec-conventions document — see *Spec-Conventions Partial* below |
| `spec_header.md` | Render spec label/work-root context supplied by the phase |
| `companions_context.md` | List companion paths declared on the spec(s) in scope |
| `scratchpad.md` | Pin the per-session scratchpad path |
| `skill_index.md` | Target v1 partial that renders the compact skill index produced by `loom-skills`: skill `name`, `description`, and paths when disclosure mode requires them. Full skill bodies are not pinned into the prompt. |
| `progress_markers.md` | Document `LOOM_COMPLETE` success and the loop-only `LOOM_NOOP` empty-diff success terminator. **Not pinned in `todo.md`** because todo success is the typed `LOOM_TODO:` payload, not a generic complete/no-op marker. |
| `todo_success.md` | Document the todo-specific success terminator `LOOM_TODO: <json>` and the `loom-protocol::todo::TodoSuccess` shape. Pinned only by `todo.md`. |
| `self_report_markers.md` | Document worker-phase cannot-finish terminators `LOOM_RETRY`, `LOOM_CLARIFY`, `LOOM_BLOCKED`. Pinned in worker phases (`todo`, `loop`, `review`) only. |
| `options_format.md` | Carry the canonical `## Options — <summary>` / `### Option N — <title>` markdown block consumed by `loom inbox`'s chat-drafter, per [gate.md § Options Format Contract](gate.md#options-format-contract). |
| `findings_walk.md` | Sole carrier of the `LOOM_FINDING:` / `LOOM_CONCERN:` colon-suffixed review wire format per [gate.md § Findings and Minting](gate.md#findings-and-minting). Pinned only by `review.md`; an anti-drift verifier fails any other template that restates the wire format. |
| `chat_marker_final_turn_only.md` | Restrict interactive-session terminal markers to the **final** assistant turn. `plan` may emit `LOOM_COMPLETE`; `inbox` may emit `LOOM_COMPLETE` or `LOOM_APPLY: {"proposals":[...]}`. Included by `plan`; pending for `inbox`. |
| `interview_modes.md` | Describe the "one by one" / "polish the spec" interview sub-modes |
| `chat_interview.md` | Interactive-session discipline for `plan` and target v1 `inbox`: conversational prose Q&A only, no Claude Code option-picker / `AskUserQuestion` widget, and phase-authorized durable destinations for anything that needs to outlive the session — see *Chat Discipline* below |
| `decomposition_discipline.md` | Pin the audit-before-fan-out and exact-roster rule on `todo`: every changed spec from driver preflight must be represented in `LOOM_TODO`, and every bead must correspond to evidence-confirmed missing work — see *Decomposition Discipline* below |
| `plan_stage_rubric.md` | Gate the planning interview on completeness / coherence / invariant-clash before any commit. Carries the pending-modifier discipline prominently — see *Planning-Rubric Pending Discipline* below. |
| `invariant_clash.md` | Describe the invariant-clash awareness scan (included transitively via `plan_stage_rubric.md`) |
| `review_rubric.md` | Finite-diff / push-range review rubric — see [gate.md](gate.md) |
| `sibling_spec_editing.md` | Authorize cross-spec edits during a planning session |

### Style-Rules Partial

The `style_rules.md` partial is **rule-family-agnostic**: it
instructs the agent to discover rule families from the pinned
`{{ style_rules }}` document, not from a fixed prefix list. The
template body never enumerates specific prefixes like `RS-` or
`COM-`; downstream consumers of loom maintain their own
`style-rules.md` with their own conventions, and the partial
adapts.

The same agnosticism applies to the `review_rubric.md` partial in
[gate.md](gate.md)'s style-rule-conformance dimension:
the rubric instructs the judge to walk every rule family the pinned
document defines, without enumerating prefixes. Any rule-ID example
in template prose is illustrative (placeholder), not normative.

### Spec-Conventions Partial

The `spec_conventions.md` partial pins
[`docs/spec-conventions.md`](../docs/spec-conventions.md), which
defines what a spec is, what it isn't, and the relationship to
code / verifiers / notes / beads. Planning sessions read it so
authored content complies with the convention; this prevents
implementation leakage, status indicators, and historical
narrative from drifting back into spec markdown.

### Pinning Policy

Each partial is included by an explicit set of templates. **Cell
vocabulary**: `✓` (partial is transitively `{% include %}`'d by
this template), blank (partial is NOT included), `?` (pending
addition), `~` (pending removal). Pending cells silent-pass during
the pending window per [gate.md § Pending support in structured
walker input](gate.md#pending-support-in-structured-walker-input).

| Partial | `plan` | `todo` | `loop` | `review` | `inbox` |
|---|:-:|:-:|:-:|:-:|:-:|
| `context_pinning.md` | ✓ | ✓ | ✓ | ✓ | ? |
| `style_rules.md` |  |  | ✓ | ✓ |  |
| `spec_conventions.md` | ✓ |  |  |  |  |
| `spec_header.md` | ? | ? | ✓ | ✓ |  |
| `companions_context.md` | ✓ | ✓ | ✓ | ✓ | ? |
| `scratchpad.md` | ✓ | ✓ | ✓ | ✓ | ? |
| `skill_index.md` | ✓ | ✓ | ✓ | ✓ | ? |
| `progress_markers.md` | ✓ |  | ✓ | ✓ |  |
| `todo_success.md` |  | ✓ |  |  |  |
| `self_report_markers.md` |  | ✓ | ✓ | ✓ |  |
| `findings_walk.md` |  |  |  | ✓ |  |
| `options_format.md` |  | ✓ | ✓ | ✓ |  |
| `chat_marker_final_turn_only.md` | ✓ |  |  |  | ? |
| `interview_modes.md` | ✓ |  |  |  |  |
| `chat_interview.md` | ✓ |  |  |  | ? |
| `decomposition_discipline.md` |  | ✓ |  |  |  |
| `plan_stage_rubric.md` | ✓ |  |  |  |  |
| `invariant_clash.md` | ✓ |  |  |  |  |
| `review_rubric.md` |  |  |  | ✓ |  |
| `sibling_spec_editing.md` | ✓ |  |  |  |  |

Pending cells mark planned include-graph updates whose prompt code has
not landed yet. The walker permits those cells while absent and reports
them as stale once the include graph catches up.

**`style_rules.md` is pinned only in `loop` and `review`** — the two
phases that write or evaluate code. Other phases don't write or
evaluate code, so pinning the rules there would inflate prompt size
without buying enforcement.

**`spec_conventions.md` is pinned only in `plan`** — the phase that
authors spec content. Other phases consume specs but don't modify them.

**`decomposition_discipline.md` and `todo_success.md` are pinned only in
`todo`** — the phase that authorizes bead creation. The driver has
already computed the changed-spec set; the prompt's job is to decompose
that exact set, report `Decomposed` or `NoWork` for every changed spec,
and emit `LOOM_TODO:` as the success marker.

### Template Variables

Each variable is bound to a typed field on the relevant context struct.
`String`-typed values arriving from beads or config flow through the
parse-don't-validate boundary defined in [harness.md](harness.md#parse-dont-validate).

| Variable | Type | Used By |
|----------|------|---------|
| `pinned_context` | `String` | all |
| `style_rules` | `String` | `loop`, `review` |
| `spec_conventions` | `String` | `plan` |
| `anchor_labels` | `Vec<SpecLabel>` | `plan` |
| `spec_index` | `String` | `plan`, `todo` |
| `label` | `SpecLabel` | `loop`, `review` |
| `changed_specs` | `Vec<TodoChangedSpec>` | `todo` |
| `work_epic` | `BeadId` | `todo` |
| `todo_head` | `GitSha` | `todo` |
| `todo_fingerprint` | `TodoFingerprint` | `todo` |
| `spec_epics` | `Vec<SpecEpicContext>` | `todo` |
| `companion_paths` | `Vec<String>` | `plan`, `todo`, `loop`, `review`, `inbox` |
| `skill_index` | `SkillIndexMarkdown` | all agent-bearing templates |
| `implementation_notes` | `Vec<SpecImplementationNotes>` | `todo` |
| `criterion_status` | `Vec<CriterionStatus>` | `todo` (see *Criterion-Status Surface* below) |
| `inbox_items` | `Vec<InboxItem>` | `inbox` |
| `molecule_id` | `Option<MoleculeId>` | `loop` |
| `issue_id` | `Option<BeadId>` | `loop` |
| `title` | `Option<String>` | `loop` |
| `description` | `Option<String>` | `loop` |
| `previous_failure` | `Option<PreviousFailure>` | `loop` (retry only; typed enum — see *Typed `PreviousFailure`* below) |
| `review_notes` | `Option<String>` | `loop` |
| `attempt` | `u32` | `loop` |
| `beads_summary` | `Option<String>` | `review` |
| `base_commit` | `Option<String>` | `review` |
| `scratchpad_path` | `String` | all |

The newtypes (`SpecLabel`, `MoleculeId`, `BeadId`, `GitSha`,
`TodoFingerprint`, `CriterionId`) are architecture-bearing parse-boundary
types. `GitSha`, `TodoFingerprint`, and the todo success protocol live in
`loom-protocol::todo`; `SpecLabel`, `MoleculeId`, and `BeadId` are defined
in [harness.md](harness.md#parse-dont-validate). The template treats them
as opaque typed values.

`implementation_notes` is sourced from `.loom/cache.db`'s `notes` table
(kind = `implementation`); see *Notes lifecycle* in
[harness.md](harness.md#sqlite-cache-store).

`skill_index` is generated by `loom-skills` after discovery, duplicate/override
resolution, phase/profile filtering, materialization, and backend disclosure
selection. The template layer receives a prompt-ready `SkillIndexMarkdown`
newtype rather than raw skill records; it renders the value through
`partial/skill_index.md` and does not inspect source/provenance. Native
registration status, source hashes, and override metadata are logged in
workflow/manifests, not rendered in normal prompts.

### Skill-Index Partial

`partial/skill_index.md` is included by every agent-bearing template. It is the
only workflow-template location that tells an agent how to discover dynamic
skills. The partial must preserve the templates/skills boundary:

- It lists compact skill entries only; full skill bodies are loaded on demand.
- In native-registered mode, entries contain `name` + `description` and instruct
  the agent to use its native skill mechanism. Paths appear only when
  `[skills].show_paths = "always"`.
- In prompt-disclosure mode, entries contain `name` + `description` + `path` and
  instruct the agent to read the path when the skill is relevant.
- It states that skills are additive strategy guidance and cannot override phase
  protocol, terminal markers, or gate requirements.

### Criterion-Status Surface

`criterion_status` is the per-criterion record that gives the `todo`
decomposition agent evidence of which Success-Criteria bullets already
have current verifier evidence before it fans out beads. The driver builds
it by parsing the changed specs' Success Criteria, computing typed
criterion ids, and joining against `.loom/cache.db`'s criterion evidence
cache. Cache absence is represented as missing evidence, never as no work.

```rust
pub struct CriterionStatus {
    pub spec_label: SpecLabel,
    pub criterion_id: CriterionId,
    pub criterion_text: String,
    pub annotation: CriterionAnnotation,
    pub evidence: EvidenceState,
}

pub struct CriterionId(/* opaque */);

pub struct CriterionAnnotation {
    pub tier: AnnotationTier,
    pub target: AnnotationTarget,
    pub pending: bool,
}

pub enum AnnotationTier {
    Check,
    Test,
    System,
    Judge,
}

pub struct AnnotationTarget(/* opaque */);

pub enum EvidenceState {
    Current {
        result: CriterionResult,
        last_timestamp_ms: i64,
        last_commit: GitSha,
        commits_since: u32,
    },
    Missing,
    StaleAnnotation {
        cached_annotation: CriterionAnnotation,
        last_timestamp_ms: i64,
        last_commit: GitSha,
        commits_since: u32,
    },
}

pub enum CriterionResult {
    Pass,
    Fail,
    Skipped,
}
```

`CriterionId` identifies the requirement, not the verifier binding. The
parser computes it from canonical bytes containing `spec_label` plus the
normalized criterion text (bullet marker stripped, continuation lines
joined with single spaces, surrounding whitespace trimmed, internal
whitespace collapsed, annotation line excluded). It deliberately excludes
annotation tier and target so changing `[check]` to `[test]` does not make
a new requirement id. Stale verifier evidence is represented by
`EvidenceState::StaleAnnotation` instead. Duplicate normalized criterion
text inside one spec is an integrity error because it would collide.

Criteria with no annotation, multiple annotations, or malformed annotation
syntax block todo preflight. They do not appear as normal
`CriterionStatus` rows because the acceptance surface is broken.

### Todo Success Marker

`partial/todo_success.md` instructs the agent that a successful todo
session ends with exactly one final line:

```text
LOOM_TODO: {"head":"<sha>","fingerprint":"<fingerprint>","work_epic":"<bead-id>","specs":[...]}
```

The JSON shape is derived from `loom-protocol::todo::TodoSuccess` as
specified in [harness.md § Spec and Work Epic Lifecycle](harness.md#spec-and-work-epic-lifecycle).
The template tells the agent to include exactly the changed specs the
driver injected, using `Decomposed { beads }` for non-empty work and
`NoWork { reason }` for an audited no-implementation outcome. `Blocked`,
`pending`, or omitted specs are not success states; the agent emits
`LOOM_CLARIFY` or `LOOM_BLOCKED` instead.

### Typed `PreviousFailure`

`previous_failure` is a typed tagged enum. The driver populates the
right variant based on the verdict-gate cause classification; the
template renders each variant with distinct framing so the agent
sees a cause-appropriate prompt rather than a one-shape blob.

```rust
pub enum PreviousFailure {
    /// Fixed-shape driver-procedural failures.
    DriverNotice { cause: DriverNoticeCause, detail: String },

    /// One or more [check]/[test]/[system] verifier failures.
    VerifyFailures(Vec<VerifierFailure>),

    /// Review LLM flagged one or more concerns. `summary` is the
    /// parsed `summary` field from the terminal
    /// `LOOM_CONCERN: {"summary": "..."}` marker; `findings` is the
    /// buffered list of streamed `LOOM_FINDING:` records (typed
    /// `Finding` per [gate.md § Findings and Minting](gate.md#findings-and-minting)).
    ReviewConcern { summary: String, findings: Vec<Finding> },

    /// Review walk's terminal signal was malformed or mismatched
    /// with the streamed-findings count. Carries the typed
    /// `BadWalk` variant; see [harness.md § Verdict Gate](harness.md#verdict-gate)
    /// for the per-variant recovery-prompt framing.
    BadWalk(BadWalk),

    /// Pre-verifier build/compile failure (agent's code didn't compile).
    BuildFailure { stage: String, output: String },

    /// Worker emitted LOOM_COMPLETE / LOOM_NOOP but left the working
    /// tree dirty (modified-but-not-staged, staged-but-not-committed,
    /// or untracked outside the ignore set). Paths capped at 30
    /// entries by the driver before construction.
    TreeNotClean { dirty_paths: Vec<String> },

    /// Bead-workspace self-check may have passed, but the loom-workspace
    /// per-bead integration step's `loom gate verify` against the
    /// integrated tree failed (cross-bead interaction, rebase-induced
    /// breakage, integration-tree state no bead-workspace verify could
    /// anticipate). The integration was rolled back via
    /// `git reset --hard HEAD~1`. Carries the verifier-failure list
    /// and durable gate-log path directly. Review concerns are produced
    /// by the molecule-completion review and route through
    /// `ReviewConcern` or `BadWalk` rather than this variant.
    PostIntegrateFail {
        failures: Vec<VerifierFailure>,
        gate_log_path: PathBuf,
    },

    /// Worker phase emitted `LOOM_RETRY` — the agent self-reported
    /// that this attempt could not finish but a fresh dispatch is
    /// likely to succeed (environmental failure: tools failing
    /// mid-session, sandbox/cwd unlinked, transient IO; or agent
    /// self-reset: stuck-but-not-blocked, prompt-context exhausted,
    /// approach abandoned). `reason` is the prose the agent wrote on
    /// the line before the marker, captured verbatim. Distinct from
    /// `DriverNotice::ObserverAbort` and from `BuildFailure` because
    /// the agent itself acknowledged the failure rather than the
    /// driver inferring it. Consumes one slot in
    /// `[loop] max_retries`; exhaustion escalates to `loom:blocked`
    /// with cause `retry-exhausted`.
    AgentRetry { reason: String },
}

pub enum DriverNoticeCause {
    SwallowedMarker,
    IncompleteSignaling,
    ZeroProgress,
    ObserverAbort,
    RetryExhausted,
    UnbondedOrigin,
}

pub struct VerifierFailure {
    pub target: String,       // e.g. "cargo test ... -- my_test"
    pub exit_code: i32,
    pub stderr_tail: String,  // ~last 40 lines, capped per-block
}

pub enum BadWalk {
    /// `LOOM_CONCERN:` payload did not parse as
    /// `{"summary": "<non-empty>"}` — invalid JSON, missing
    /// `summary` field, or empty `summary`. The literal post-marker
    /// text is preserved for the recovery prompt, AND any
    /// well-formed `LOOM_FINDING:` lines that streamed ahead of
    /// the bad terminator are preserved in `parsed_findings` so the
    /// agent's diagnosis is not lost when only the terminal was
    /// malformed.
    Concern { payload: String, parsed_findings: Vec<Finding> },

    /// Terminator claimed concern but zero `LOOM_FINDING:` lines
    /// streamed during the walk. The parsed summary is preserved
    /// so the recovery prompt can quote it back.
    ConcernWithoutFindings { summary: String },

    /// One or more `LOOM_FINDING:` lines streamed but the
    /// terminator was `LOOM_COMPLETE`. The count AND the parsed
    /// findings are preserved so the next iteration's prompt can
    /// name them, and so `loom gate mint` can consume the same
    /// records on the next walk rather than re-deriving them.
    FindingsWithoutConcern { finding_count: usize, findings: Vec<Finding> },

    /// One or more `LOOM_FINDING:` lines failed parse (most
    /// common: trailing backticks from markdown fencing on an
    /// otherwise-valid JSON payload). `errors` is one
    /// `FindingParseError` per malformed line. `terminal` is the
    /// well-formed terminator (or its typed
    /// `Missing`/`Malformed` placeholder) so the agent's next
    /// iteration sees BOTH the per-line malformation detail AND
    /// the surrounding well-formed context that was preserved.
    MalformedFinding { errors: Vec<FindingParseError>, terminal: TerminalSurface },
}

/// Typed projection of the agent's terminal marker, mirroring
/// `ExitSignal` but with explicit malformed/missing variants so
/// `BadWalk::MalformedFinding` can carry the terminal state
/// regardless of whether the terminal itself parsed.
pub enum TerminalSurface {
    Complete,
    Noop,
    Concern { summary: String },
    Retry { reason: String },
    Blocked { reason: String },
    Clarify { question: String },
    Malformed { payload: String },
    Missing,
}
```

`FindingParseError` is re-exported from `loom-workflow::review::finding`
(per [gate.md § Findings and Minting](gate.md#findings-and-minting)) —
the typed wire-format error the parser produces. Carrying a
`Vec<FindingParseError>` in `BadWalk::MalformedFinding` means each
per-line malformation rides through with its `line_number`, the
literal `raw` line text, and the typed reason (`Json`,
`UnknownToken`, `TokenVariantMismatch`, `UnknownBondSpec`,
`UnresolvedTarget`, `TargetSpecNotInBonds`).

**Maximum-context preservation invariant.** Every `BadWalk`
variant carries the maximum well-formed context by struct shape;
construction without the parseable pieces is a compile error. The
"lost the agent's diagnosis when one piece of the walk was
malformed" failure mode is structurally unrepresentable. See
[gate.md § Streaming + terminator pairing rule](gate.md#findings-and-minting)
for the cross-product of (stream-shape × terminal-shape) cells the
variants cover.

The per-finding concern token (the enum that names which rubric
check fired — `verifier-bypass`, `spec-coherence-fail`, etc.)
lives on each `Finding`'s `token` field per [gate.md § Concern
tokens and target variants](gate.md#concern-tokens-and-target-variants),
not on the `PreviousFailure::ReviewConcern` variant itself. The
terminal marker is verdict-log shape only; per-finding routing is
decided by `loom gate mint`.

**Caps:**

- `PREVIOUS_FAILURE_MAX_LEN = 4000` total
- Each `VerifierFailure.stderr_tail` capped individually
  (~1500 chars) before the per-variant total is split across
  multiple failures (later failures truncated first when the
  total exceeds budget)
- `review_notes` has a separate ~1000-char budget, independent
  of `previous_failure`

**Template framing.** Each variant renders distinctly:

- `DriverNotice` → `"Previous attempt: {detail}"`
- `VerifyFailures` → `"Verifier failures from previous attempt:\n\n{N blocks: target + exit + stderr}"`
- `ReviewConcern` → `"Review raised {N} concern(s) — {summary}\n\n{per-finding digest: token + evidence first line}"`
- `BadWalk(Concern { payload, parsed_findings })` → `"Your LOOM_CONCERN payload did not parse as {\"summary\": \"<non-empty>\"}. Literal payload: {payload}"`, followed (when `parsed_findings` is non-empty) by `"\n\n{N} finding(s) parsed cleanly before the malformed terminator:\n{per-finding digest: token + first line of evidence}"` so the agent's diagnosis from the streamed findings is not lost when only the terminal was malformed.
- `BadWalk(ConcernWithoutFindings { summary })` → `"You emitted LOOM_CONCERN ({summary}) but no LOOM_FINDING: lines streamed. Either emit findings before the terminator or use LOOM_COMPLETE."`
- `BadWalk(FindingsWithoutConcern { finding_count, findings })` → `"You streamed {finding_count} LOOM_FINDING line(s) but terminated with LOOM_COMPLETE. Use LOOM_CONCERN: {\"summary\": \"...\"} when findings are emitted."`, followed by `"\n\nFindings streamed:\n{per-finding digest}"` so the agent's next iteration sees the diagnosis it just emitted.
- `BadWalk(MalformedFinding { errors, terminal })` → `"One or more LOOM_FINDING: lines failed strict validation. Re-emit each finding as a single line: `LOOM_FINDING: {\"token\":\"...\",\"route\":\"blocking|deferred|clarify\",\"bonds\":[...],\"target\":{...},\"evidence\":\"...\"}`.\n{per-line: 'Line N: <reason> — raw: <line text>'}\n\nYour terminal was: {terminal-rendered}"`. The terminal rendering uses the typed `TerminalSurface` variant: `Complete` → `"LOOM_COMPLETE"`, `Concern { summary }` → `"LOOM_CONCERN: {summary}"`, `Malformed { payload }` → `"LOOM_CONCERN: <malformed: {payload}>"`, `Missing` → `"(no terminal on the final non-empty line)"`. Surfacing both pieces lets the agent fix the malformed lines (typically: add the missing `route` field or drop the surrounding markdown fence) without losing the well-formed context.
- `BuildFailure` → `"Build failed at {stage}:\n{output}"`
- `TreeNotClean` → `"Working tree was not clean after the bead committed:\n\n{path list, one per line}\n\nStage these into a follow-up commit or revert them."` with a `"+N more"` suffix line when the list is truncated to 30 entries
- `PostIntegrateFail { failures, gate_log_path }` → `"After rebasing onto the integration branch, the post-integration verify failed.\n\nGate log: {gate_log_path}\n\n{N blocks: target + exit + stderr}\n\nReconcile the cross-bead interaction — your bead's verify passed at its own workspace; the failure is in the integrated tree."`
- `AgentRetry { reason }` → `"Previous attempt requested retry — reason: {reason}\n\nIf the same problem persists after this attempt, escalate to LOOM_BLOCKED (no candidate resolutions) or LOOM_CLARIFY (with a structured Options block) rather than emitting LOOM_RETRY again."`
- `review_notes` (when set, after the primary block) → heading `"Review notes:"` then content

Driver maps verdict-gate causes to variants per the table in
[harness.md — Verdict Gate](harness.md#verdict-gate).

### Attempt Counter

`attempt` is the per-bead in-session retry counter, populated by
the driver and rendered by `loop.md`:

- `attempt == 0` on fresh bead dispatch — no retry context, no
  attempt line in the template
- Each in-session retry increments `attempt` (bounded by
  `[loop] max_retries`, default 2)
- Resets to 0 when a new bead is dispatched (fix-up beads carry
  fresh prompts, not retry state from the failing bead)
- **Molecule-level iteration is opaque to the agent** — fix-up
  beads are different prompt contexts, and a counter that spans
  them would be misleading

When `attempt > 0 && previous_failure.is_some()`, `loop.md`
prepends a counter line: `"Retry attempt {attempt} — previous
attempt failed with: …"` followed by the typed
`previous_failure` block.

### First-instruction reframe

When `previous_failure.is_some()`, `loop.md` prepends to its first
user instruction:

> "Re-read the previous failure block above and address its
> specific concern before re-implementing."

This single generic reframe forces the agent to acknowledge the
prior failure as actionable input rather than skim past it. The
per-variant framing (above) carries the cause-specific detail; the
top-of-prompt reframe just establishes the directive.

### Loop completion self-check and self-review

`loop.md`'s quality-gate block instructs the worker to finish by
verifying the exact injected bead range, not by relying on a working-
tree shorthand. The rendered command names
`loom gate verify --diff <bead-base>..HEAD` (or `@{u}..HEAD` only when
the upstream is the injected base), and tells the agent to rerun the
self-check after any later commit or hook-generated file change. The
prompt also requires a structured self-review before the final marker:
re-read the bead's criteria, inspect the final diff, check style/spec
fit, and either fix the issue or emit the appropriate worker self-report
marker. This is prompt-level feedback discipline; the driver-side trust
boundary remains the post-integration verify and molecule push gate in
[harness.md](harness.md).

### Agent-Output Markers

Agent-generated content rendered back into a prompt
(`previous_failure`, `title`, `description`, prior work-epic diagnostics,
implementation notes) is delimited with `<agent-output>` /
`</agent-output>` markers so the receiving agent can distinguish injected
content from system instructions. This is a best-effort prompt-injection
mitigation; the real trust boundary is the container.

### Chat Discipline

`partial/chat_interview.md` is included by every interactive-session
template: `plan.md` and `inbox.md`. It carries the discipline shared
across every interactive session the loom binary runs with a human in
the loop:

- Questions go out in prose, in the assistant's normal reply.
  Answers come back as user prose.
- The agent does **not** use Claude Code's structured option-picker
  tool (`AskUserQuestion` or any equivalent multi-choice widget) for
  interactive sessions. The picker forces premature commitment to N
  enumerated options when the user's real answer may be a hybrid, a
  redirection, or none-of-the-above; it also adds friction to the
  short text replies that are the natural shape of conversational
  discussion.
- When the agent wants to propose alternatives, it lists them
  inline in prose ("option A does X; option B does Y"). The user
  replies "B" or "B with a tweak" or "neither, do Z" — natural
  prose, no picker UI.
- **Persistence destinations.** Session-bridging memory — decisions,
  context, follow-ups, anything future sessions need — goes only to the
  durable surface this phase authorizes. In `loom plan`, durable
  planning output goes in spec/index markdown or implementation notes;
  plan does not write bd. In `loom inbox`, bd notes/descriptions are the
  authorized resolution surface. Claude Code's `MEMORY.md` / auto-memory
  system is container-local and disappears with the container; treat it
  as working notes for the current session only, not as durable storage.
- The "one by one" sub-mode (see *Interview Modes*) is planning-
  specific and lives in a separate partial; the chat-discipline rules
  above apply to every interactive session, including inbox-chat.

Worker phases (`loop`, `todo`, `review`) are single-shot and do not
interview the user, so the partial is not pinned there.

### Decomposition Discipline

`partial/decomposition_discipline.md` is included by `todo.md`. It tells
the decomposition agent that the driver has already computed the exact
changed-spec roster and created or reused the `loom:todo` work epic. The
agent must decompose **that roster exactly**; it does not discover or
narrow the changed-spec set.

Before authoring any non-audit bead, the agent must:

1. Consult the `criterion_status` surface (see *Criterion-Status Surface*)
   for each criterion in each changed spec. `EvidenceState::Current { result:
   Pass, commits_since: 0, ... }` is positive evidence of coverage;
   `Missing` or `StaleAnnotation` is absence/staleness of evidence, not a
   reason to treat the criterion as already complete.
2. Read representative existing implementations and verifier functions for
   criteria where evidence is missing, stale, failed, skipped, or the agent
   judges the verifier target may not exercise the live system per
   [spec-conventions.md](../docs/spec-conventions.md)'s "no tier-skipping"
   rule. A directory listing proves a file exists; it does not prove the
   file contains the named target.
3. Create implementation beads only under the injected `work_epic`, and
   label/bond each bead to the spec(s) it implements. Beads outside the work
   epic cannot satisfy `LOOM_TODO` validation.

A successful `loom todo` session has exactly one success outcome: emit
`LOOM_TODO: <json>` on the final line. The JSON must report every changed
spec exactly once, with `Decomposed { beads }` for specs that produced
non-empty work and `NoWork { reason }` for specs audited as requiring no
implementation change (for example typo-only spec wording). The agent may
not omit changed specs, report a pending state as success, or use
`LOOM_COMPLETE` / `LOOM_NOOP` as todo success.

Decision-needed or dead-end outcomes use worker self-report markers:

- **Clarify on the work epic.** When coverage cannot be determined by
  inspection — spec ambiguity, conflicting verifier targets, cursor/index
  inconsistency needing human choice, or contestable cache trust — the agent
  emits `LOOM_CLARIFY` with the question and `## Options — …` block
  persisted to the **`loom:todo` work epic's** notes/description per the
  *Options Format Contract* in [gate.md](gate.md). The verdict gate applies
  `loom:clarify` to that work epic; the human resolves via `loom inbox`, and a
  subsequent `loom todo` invocation reuses the matching pending work epic.
- **Blocked on the work epic.** When the agent has no candidate resolutions
  to enumerate, it emits `LOOM_BLOCKED`; the work epic remains non-active
  and spec cursors do not advance.

Per-bead `loom:clarify` is not appropriate in todo because the child beads
under negotiation may not exist yet, or may be exactly the set whose
validity is disputed. The work epic is the session-stable carrier for
"this decomposition batch is paused pending clarification".

**Work-epic-first always.** The driver creates or reuses the `loom:todo`
work epic before rendering `todo.md`, so clarify/block paths always have a
valid target and the agent never has to create the batch container.

**Enumerate-everything defaults are forbidden by data, not by grep.** A
fixed decomposition axis — e.g. "setup, implementation, tests,
documentation" applied across the board irrespective of evidence — is the
failure mode this discipline targets. The combined effect of (i) typed
criterion evidence exposing current/missing/stale verifier state and (ii)
the exact-roster `LOOM_TODO` validator makes such fan-outs structurally
unviable. `loom gate review`'s judge-tier walk catches any decomposition
that bypasses the evidence surface to re-introduce enumerate-everything
beads.

**Template-agnostic.** The partial describes the audit obligation in terms
of "changed specs", "criteria in scope", and "representative
implementations", not specific file paths or crate names. Downstream
consumers of loom whose workspace layouts differ from this one inherit the
same discipline against their own layouts.

### Review Emit Shape

`review.md` is the LLM-rubric walk's prompt template. The reviewing
agent emits findings as streaming `LOOM_FINDING:` lines on stdout —
one line per finding, identified as the walk proceeds, with a JSON
payload after the prefix:

```
LOOM_FINDING: {"token": "...", "route": "blocking|deferred|clarify", "bonds": ["..."], "target": {"kind": "...", ...}, "evidence": "..."}
```

Followed by exactly one terminal marker:
`LOOM_COMPLETE` (zero findings emitted),
`LOOM_CONCERN: {"summary": "<one sentence>"}` (≥1 findings emitted —
JSON-shaped payload, parsed by the same `serde_json` pipeline
consuming the `LOOM_FINDING:` lines),
`LOOM_RETRY` (the walk could not complete for environmental reasons
— logs corrupt, workspace inaccessible, transient IO — and a fresh
dispatch should retry it; per [harness.md § Verdict
Gate](harness.md#verdict-gate) this consumes one
`[loop] max_retries` slot), `LOOM_BLOCKED` (the walk cannot complete
and the reviewer has no candidate resolution to enumerate), or
`LOOM_CLARIFY` (the walk surfaces a spec ambiguity the reviewer can
frame as `## Options — …` for human resolution).
The terminator must satisfy the **pairing rule**: `LOOM_CONCERN`
iff ≥1 findings streamed, `LOOM_COMPLETE` iff zero — a mismatch
routes to `RecoveryCause::BadWalk(BadWalk)` per [harness.md §
Verdict Gate](harness.md#verdict-gate). All review-walk
wire-format text lives in the `findings_walk.md` partial; the
review template `{% include %}`s it rather than restating, and a
`[check]`-tier anti-drift verifier enforces this mechanically per
[gate.md § Findings and Minting](gate.md#findings-and-minting).

The `route` field on each `LOOM_FINDING:` drives per-finding
workflow (`blocking`, `deferred`, or `clarify`). The `bonds` array
names the spec(s) the fix-up should bond to (bonding info); the
`target` carries identity-bearing fields specific to the variant.
JSON was chosen over pipe-delimited shapes because LLM emit is more
reliable on JSON, and the tagged-union encoding of `target` is
naturally JSON-shaped. The terminator's `summary` is a verdict-log
entry only — per-finding routing is decided from the streamed line's
`route`, not from the terminal token.

**The review template makes no bd writes.** Earlier revisions of
this spec authorized `bd create` / `bd update` / `bd mol bond` from
inside the review prompt — those instructions are removed. The
agent's job is to identify findings and emit them; the driver
(`loom gate mint`) is the sole chokepoint that mints fix-up beads
from the typed `LOOM_FINDING:` lines, applying fingerprint dedup
and per-spec molecule resolution. A review run that mutates bd
state from inside the prompt is a protocol violation.

**Clarify-bound findings embed Options in evidence.** Any finding
whose mint would label the resulting bead `loom:clarify` —
`invariant-clash` and any other clarify-bound token enumerated by
[gate.md § Concern tokens and target variants](gate.md#concern-tokens-and-target-variants)
— MUST embed the canonical `## Options — <summary>` block inside
its `evidence` payload (with at least one `### Option <N> — <title>`
subsection). The driver-side `loom gate mint` parses the evidence,
extracts the block, and renders it into the minted clarify bead's
description per the *Options Format Contract*. A clarify-bound
finding whose evidence lacks a well-formed options block is refused
at mint time and falls back to `loom:blocked` with cause
`clarify-without-options`; the agent should emit `LOOM_BLOCKED`
directly when it cannot articulate options rather than emitting a
clarify-bound finding without them.

### Mint Default-Profile

The per-spec default profile (`profile:rust` for cargo-bound specs;
`profile:base` for Nix-only and unknown specs) is consumed by the
driver-side `loom gate mint` flow when it issues `bd create
--labels=…` for fix-up and clarify beads. The mapping is
`default_profile_for_spec(&SpecLabel)` in
`loom-workflow::review::context`; cargo-bound specs (`harness`,
`templates`, `agent`, `gate`, `llm`, `tests`) resolve to
`profile:rust` so the fix-up bead's dispatch container has the Rust
toolchain its `[check]` / `[test]` verifiers need; Nix-only specs
(currently `pre-commit`) and unknown specs stay on `profile:base`.
Mint applies this default to every fix-up it creates; the operator
overrides via `bd update <id> --labels` post-mint when a specific
fix-up's toolchain needs diverge from the spec's default.

This was previously a `review.md` template concern (`bd create
--labels=…` examples were rendered with `profile:{{ default_profile }}`).
After unification, `review.md` no longer emits `bd create` calls;
the driver applies the default profile when minting from
`LOOM_FINDING:` lines.

### Planning-Rubric Pending Discipline

`partial/plan_stage_rubric.md` is pinned in `plan.md`. It owns the
planning interview's pre-commit gate (completeness / coherence /
invariant-clash) **and** the pending-modifier discipline that
determines whether the planning session's spec edits can pass the push
gate.

The partial body MUST spell out the pending-modifier discipline
unambiguously, because the planning session's biggest failure mode
is *spec edits that point at not-yet-existing verifier targets,
which then fail the pre-push `loom gate verify` and block landing
the plan*. The discipline lives in [gate.md § Pending
modifier](gate.md#pending-modifier) and its sub-rule [gate.md §
Pending support in structured walker input](gate.md#pending-support-in-structured-walker-input)
— the partial body distills both for the planning agent with the
following clauses, each grep-able by an integrity verifier so the
partial cannot quietly drift:

1. **Both binary-pending AND assertion-pending are pending.** The
   partial enumerates both shapes explicitly:
   - **Binary-pending** — the verifier executable or path doesn't
     exist yet (e.g. `[check?](cargo run -p my-future-walker ...)`,
     or `[check?](grep -q ... crates/foo/src/file_that_will_exist.rs)`).
   - **Assertion-pending** — the verifier executable exists but the
     asserted condition doesn't hold yet (e.g. `[check?](grep -q
     'pub enum NewVariant' crates/foo/src/existing_file.rs)` where
     the file exists but the new symbol hasn't been added).

   Both shapes use the same `?` modifier; both silent-pass under
   `loom gate verify` and fire `UnneededPendingMarker` once the
   target newly resolves.

2. **"Added" and "modified" annotations both count.** The partial
   names this explicitly, with a worked example: *"if you changed
   an annotation's command — a file path, a grep pattern, a symbol
   name — and the new target doesn't resolve in the current tree,
   mark it `[tier?]` even though the annotation itself isn't new.
   The integrity gate doesn't distinguish 'new claim' from 'modified
   claim'; it checks whether the target resolves now."* This
   prevents the failure mode where a planning agent only `?`-marks
   net-new SCs and forgets that path swaps on existing SCs need it
   too.

3. **Structured walker input uses `?` and `~` cells.** When the
   planning session edits structured input read by a sweeping
   walker (the pinning-matrix cell values, the FR1 command-set
   entries the surface-conformance walker reads, the canonical-
   partial path the anti-drift wire-format walker reads), the
   pending value is `?` (pending addition — will resolve to the
   present marker) or `~` (pending removal — will resolve to the
   absent marker) *in the input element itself*, not in the SC
   annotation. Per [gate.md § Pending support in structured walker
   input](gate.md#pending-support-in-structured-walker-input),
   the walker silent-passes pending elements whose state matches
   the pending direction and fires `pending-marker-resolved` when
   the state catches up. This is the structural answer to the
   sweeping-walker case; the partial body cites the gate.md rule
   and walks the agent through identifying which spec edits affect
   structured walker input.

4. **Self-cleaning is mandatory.** When the implementation lands,
   the pending marker (`?` for `[tier?]` annotations, `?` or `~`
   for structured walker cells) must be dropped in the same diff
   that resolves the target — `UnneededPendingMarker` for
   annotations, `pending-marker-resolved` for structured walker
   cells. The planning prompt names this so the agent doesn't
   author pending markers as fire-and-forget.

The partial body's text follows the standard one-line-per-rule
shape pinned by the other discipline partials (`chat_interview.md`,
`decomposition_discipline.md`); each numbered clause above maps to
a labelled paragraph in the partial body that the `loom gate check`
walker greps for.

### Sibling-Spec Editing

`partial/sibling_spec_editing.md` is included in `plan.md`. It tells
the planning agent:

1. Any labels passed to `loom plan [SPEC_LABEL ...]` are **anchors**:
   they seed initial context only and do not define the touched set.
2. During this session, the agent may read and edit any spec in
   `specs/` when a change cross-cuts sibling specs. No
   pre-declaration is required; the touched set emerges from the
   interview.
3. **Creating a new sibling spec is a valid outcome** when the
   planner judges that a section warrants its own spec. The planner
   creates `specs/<label>.md` and records its index entry in
   `docs/README.md`; it does **not** allocate a bead/epic. `loom todo`
   creates the spec epic and work epic later during deterministic
   preflight.
4. **Commits are never automatic.** Planning sessions edit specs
   in place but do not commit. Soft signals ("looks good",
   "accept") authorize the next interview step, not a commit.
   Commits happen only on unambiguous trigger ("commit", "land the
   plane", "push it"). The same discipline applies to `git push`,
   `beads-push`, and any operation that mutates shared state.

### Public Surface for Consumers

`templates` is a public-contract crate. External Rust
consumers (e.g. RAG pipelines, domain-specific review tools)
depending on `llm` for typed LLM calls compose their own
templates from `templates`' exposed building blocks:

**Exposed typed context structs:**

- `PinnedContext` (the project-overview + style-rules pinning shape)
- `PreviousFailure`, `VerifierFailure`, `BadWalk`,
  `DriverNoticeCause` (the typed retry-context surface). The
  per-finding `Finding` record carried inside
  `PreviousFailure::ReviewConcern` is owned by `loom-workflow`
  (per [gate.md § Findings and Minting](gate.md#findings-and-minting))
  and re-exported here as a typed dependency.
- `CriterionStatus`, `EvidenceState`, `CriterionId`,
  `CriterionAnnotation` (the decomposition-phase criterion-evidence
  surface; consumers writing decomposition-style tools reuse this shape
  against their own caches)
- `PlanContext`, `TodoContext`, `LoopContext`, `ReviewContext`
  (workflow-phase context shapes consumers can either reuse directly or
  model their own contexts after)

**Exposed partial strings:**

Each partial in the *Partials* table above is also available as
a public `pub const` `&'static str` so consumers can `include!` or
`{% include %}` them in their own templates:

```rust
pub const SCRATCHPAD_PARTIAL: &str = include_str!("templates/partial/scratchpad.md");
pub const CONTEXT_PINNING_PARTIAL: &str = include_str!("templates/partial/context_pinning.md");
// ...
```

**Stability guarantees:**

- Typed context struct field additions are minor version bumps
  (additive)
- Removing or renaming fields is a major bump
- Partial body changes are minor bumps (consumers don't
  destructure the body)
- Partial *path* renames (e.g. `scratchpad.md` → `scratch.md`) are
  major bumps because consumers reference the partial name

**Not exposed:**

- The compiled Askama machinery itself — consumers bring their
  own template engine (Askama, minijinja, raw `format!`, etc.)
  for their own templates
- Loom's workflow templates (`plan.md`, `todo.md`, `loop.md`,
  etc.) — consumers cannot override these; Loom's workflow
  shape is opinionated and ships with the binary

### Snapshot Test Contract

Every template × representative-input combination has an `insta`
snapshot. The rendered body is the contract shipped to the agent;
layout drift slips past substring assertions. Snapshots surface
diffs in PR review. Updates require an explicit
`snapshot updated because: <reason>` line in the PR description
(per the team's testing rules).

## Configuration

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

All three are project-relative paths. Empty values are rejected at
config parse time as `ConfigError::EmptyPath { field }` — blanking
a config does not disable the pin. To genuinely drop a pin, remove
the corresponding `{% include %}` from the relevant template (a
spec change, not a config one). Defaults keep the bundled
documents in front of the agent with zero configuration.

## Success Criteria

### Engine

- All workflow templates compile under Askama with their typed
  context structs
  [check](cargo build -p loom-templates)
- Each template has a typed context struct with every variable
  in the template body bound as a field
  [test](template_renders_are_byte_stable_across_runs)
- Templates compile at build time — missing variables are compile
  errors, not runtime errors
  [test](template_renders_are_byte_stable_across_runs)
- Partials are included via Askama's `{% include %}` mechanism
  [check](grep -q 'partial/context_pinning' crates/loom-templates/templates/loop.md)
- Rendered output is stable across runs for identical inputs,
  verified by `insta` snapshots
  [test](template_renders_are_byte_stable_across_runs)
- Template bodies must not name harness subcommands the spec marks
  removed (`loom run`, `loom check <X>` — see *Removed surface* in
  [harness.md](harness.md)); the rename targets are `loom loop` and
  `loom gate <X>`. Drift breaks every plan / todo / loop / inbox /
  review session by directing the agent at non-existent dispatch
  (Invariant 3 from [gate.md](gate.md))
  [check](cargo run -p loom-walk -- templates_no_removed_surface)

### Pinning policy

- `style_rules.md` partial renders the `style_rules` variable
  [check](grep -q '{{ style_rules' crates/loom-templates/templates/partial/style_rules.md)
- `loop.md` and `review.md` include `style_rules.md`; no other phase
  template does
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- `spec_conventions.md` partial renders the `spec_conventions` variable;
  included only by `plan.md`
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- `todo_success.md` exists, is included only by `todo.md`, and names the
  `LOOM_TODO:` success marker plus the `TodoSuccess` Rust type
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- `todo.md` deliberately omits `progress_markers.md`; generic
  `LOOM_COMPLETE` / `LOOM_NOOP` are wrong-phase success markers for todo
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- `LoopContext` and `ReviewContext` carry `style_rules: String`; other
  phase contexts do not
  [check](cargo test -p loom-templates --test render template_renders_are_byte_stable_across_runs)
- `PlanContext` carries `spec_conventions: String`; other phase contexts
  do not
  [check](cargo test -p loom-templates --test render template_renders_are_byte_stable_across_runs)
- `LoomConfig.style_rules` defaults to `"docs/style-rules.md"`;
  `LoomConfig.spec_conventions` defaults to
  `"docs/spec-conventions.md"`; `LoomConfig.pinned_context` defaults to
  `"docs/README.md"`
  [test](pin_paths_default_to_bundled_docs)
- Empty string values for any pin path are rejected at parse time with
  `ConfigError::EmptyPath { field }` naming the offending field
  [test](empty_pin_path_returns_empty_path_error)
- The `style_rules.md` and `review_rubric.md` partials are
  rule-family-agnostic: their bodies do not enumerate fixed prefixes like
  `SH-` / `RS-` / `COM-`; rule-ID examples in template prose are
  placeholders, not normative
  [check](cargo test -p loom-templates --test render review_renders_style_rule_conformance_walkthrough)
- Every non-pending cell of the pinning matrix above matches the actual
  `{% include %}` graph in `loom-templates/templates/` (transitive
  resolution); drift in either direction fails the audit
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- The `skill_index.md` partial is included by every agent-bearing template and
  is the only workflow-template location that describes skill discovery/loading
  semantics
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- `partial/skill_index.md` renders `{{ skill_index }}` and contains no full
  built-in skill body literals; disclosure fields are generated by
  `loom-skills`, not hard-coded in templates
  [check](grep -q '{{ skill_index' crates/loom-templates/templates/partial/skill_index.md)
- The `chat_marker_final_turn_only.md` partial is included by every
  interactive-session template (`plan`, `inbox`) and documents that inbox may
  use `LOOM_APPLY: {"proposals":[...]}` as its final marker when driver apply
  is requested
  [test](every_multi_turn_template_includes_chat_marker_partial)
- One-shot worker templates (`todo`, `loop`, `review`) deliberately omit
  `chat_marker_final_turn_only.md` because every response in those phases
  is the session's final output
  [test](worker_templates_omit_chat_final_turn_clause)
- `partial/chat_interview.md` exists and is included by every
  interactive-session template (`plan`, `inbox`) and by no worker template;
  the body forbids Claude Code's structured option-picker tool for
  interactive Q&A and requires conversational prose instead
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- The partial body names the picker prohibition explicitly so a grep for
  the rule succeeds (no rule-by-implication)
  [check](grep -qi 'option-picker\|AskUserQuestion' crates/loom-templates/templates/partial/chat_interview.md)
- The partial body names the persistence-destination clause distinctively
  so a grep for the rule succeeds: interactive sessions persist
  cross-session memory via the phase-authorized durable surface, not via
  Claude Code's `MEMORY.md` system which is container-local; plan is
  explicitly barred from bd writes while inbox can use bd notes
  [check](grep -qi 'MEMORY.md\|bd update.*--notes' crates/loom-templates/templates/partial/chat_interview.md)
- `inbox.md` rendered prompt contains the chat-interview discipline clauses
  (picker prohibition + persistence destinations) sourced from the pinned
  partial
  [test?](inbox_template_renders_chat_interview_discipline)

### Agent-output markers

- Templates that render agent-generated content delimit it with
  `<agent-output>` / `</agent-output>` markers
  [test](agent_output_markers_wrap_each_agent_supplied_field)

### Snapshot tests

- Every template × representative-input combination has an `insta`
  snapshot
  [check](cargo test -p loom-templates --test snapshots)
- Snapshot tests run under the workspace clippy test exemptions
  (no per-file `#![allow(clippy::unwrap_used, ...)]`)
  [check](cargo run -p loom-walk -- loom_templates_snapshots_no_crate_root_allow)

### Sibling-spec editing

- `partial/sibling_spec_editing.md` documents that creating a new
  sibling spec is a valid planning-session outcome, requires an index
  row, and says plan does not allocate a bead/epic
  [judge](../tests/judges/loom.sh#judge_sibling_spec_editing_documents_split)

### Pinning matrix walker pending support

- The pinning-matrix walker accepts `?` (pending addition) and
  `~` (pending removal) as valid cell values in the matrix
  alongside `✓` and blank, per [gate.md § Pending support in
  structured walker input](gate.md#pending-support-in-structured-walker-input)
  [test](template_pinning_matrix_accepts_pending_cells)
- `?` + template-doesn't-include → silent pass (pending);
  `?` + template-includes → walker fails with
  `pending-marker-resolved` so the author drops `?` to `✓` in the
  same diff
  [test](pending_addition_marker_fires_when_template_now_includes)
- `~` + template-includes → silent pass (pending);
  `~` + template-doesn't-include → walker fails with
  `pending-marker-resolved` so the author drops `~` to blank in
  the same diff
  [test](pending_removal_marker_fires_when_template_no_longer_includes)
- The walker's existing per-cell assertion is unchanged for
  non-pending cells: `✓` requires transitive include; blank
  forbids transitive include; mismatch fails the walker
  [check](cargo run -p loom-walk -- template_pinning_matrix)

### Planning-rubric pending discipline

- `partial/plan_stage_rubric.md` exists and is included by `plan.md`
  only
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- The partial body distinguishes **binary-pending** from
  **assertion-pending** pending-modifier cases with worked
  examples, so a planning agent author understands both shapes
  warrant `?`
  [check](grep -qi 'binary-pending\|assertion-pending' crates/loom-templates/templates/partial/plan_stage_rubric.md)
- The partial body names the **"added and modified" rule**
  explicitly — pending discipline applies to annotations the
  session adds AND to annotations whose target the session
  changed in a way that breaks resolution
  [check](grep -qi 'added.*modified\|added and modified\|modified.*annotation' crates/loom-templates/templates/partial/plan_stage_rubric.md)
- The partial body names the **structured walker input** rule —
  planning edits to matrix / surface / wire-format input use the
  walker's `?` (pending addition) and `~` (pending removal) cell
  syntax for pending elements, not the SC-level `?` modifier, per
  gate.md § Pending support in structured walker input
  [check](grep -qi 'structured.*input\|pending.*cell\|walker.*input' crates/loom-templates/templates/partial/plan_stage_rubric.md)
- The partial body names the **self-cleaning obligation** — the
  `?` must be dropped in the same diff that resolves the target,
  else `UnneededPendingMarker` fires
  [check](grep -qi 'UnneededPendingMarker\|self-cleaning\|drop the.*marker' crates/loom-templates/templates/partial/plan_stage_rubric.md)

### Todo success shape

- `partial/todo_success.md` is the single source of truth for the
  `LOOM_TODO: <json>` success marker and names the
  `loom-protocol::todo::TodoSuccess` type
  [check](grep -q 'LOOM_TODO:' crates/loom-templates/templates/partial/todo_success.md)
- `todo.md` includes `todo_success.md` via `{% include %}` rather than
  restating the success marker contract inline
  [check](grep -q 'partial/todo_success.md' crates/loom-templates/templates/todo.md)
- `partial/progress_markers.md` contains no `LOOM_TODO:` literal;
  todo success belongs to `todo_success.md`
  [check](bash -c "! grep -nE 'LOOM_TODO:' crates/loom-templates/templates/partial/progress_markers.md")
- Rendered `todo.md` prompts instruct the agent that `LOOM_COMPLETE` and
  `LOOM_NOOP` are wrong-phase success markers for todo
  [test](todo_template_rejects_generic_success_markers)

### Review emit shape

- `partial/findings_walk.md` is the single source of truth for the
  `LOOM_FINDING: <json>` streaming wire format and the terminal
  `LOOM_CONCERN: {"summary": "..."}` JSON shape. The partial
  documents the `{"token","route","bonds","target","evidence"}` finding
  payload with tagged `target` variants, the JSON CONCERN
  terminator, and the streaming + terminator pairing rule
  [check](grep -q 'LOOM_FINDING:' crates/loom-templates/templates/partial/findings_walk.md)
- `review.md` includes `findings_walk.md` via `{% include %}` rather
  than restating the wire format
  [check](grep -q 'partial/findings_walk.md' crates/loom-templates/templates/review.md)
- `review.md` does not contain a `bd create` invocation (the
  driver-side `loom gate mint` is the sole bd-mutation chokepoint;
  review is inspection-only)
  [check](bash -c "! grep -nE 'bd create|bd mol bond|bd update --add-label' crates/loom-templates/templates/review.md")
- `partial/progress_markers.md` covers the progress markers
  (`LOOM_COMPLETE`, loop-only `LOOM_NOOP`) and contains no `LOOM_CONCERN:` or
  `LOOM_FINDING:` literal — those belong to `findings_walk.md`
  per the partial split documented in [gate.md § Findings and
  Minting](gate.md#findings-and-minting)
  [check](bash -c "! grep -nE 'LOOM_CONCERN:|LOOM_FINDING:' crates/loom-templates/templates/partial/progress_markers.md")
- `partial/self_report_markers.md` covers the worker-phase self-report
  markers (`LOOM_RETRY`, `LOOM_CLARIFY`, `LOOM_BLOCKED`) and contains
  no `LOOM_CONCERN:` or `LOOM_FINDING:` literal
  [check](bash -c "! grep -nE 'LOOM_CONCERN:|LOOM_FINDING:' crates/loom-templates/templates/partial/self_report_markers.md")
- Interactive-session templates (`plan.md`, `inbox.md`) deliberately
  **omit** `self_report_markers.md` because the worker-phase
  cannot-finish markers are not valid emit options for interactive
  sessions — the human resolves friction in-turn. Including the partial
  would teach interactive agents about markers they cannot emit
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- The partial body names `LOOM_RETRY` semantics distinctively
  (transient / environmental / agent-self-reset, consumes a
  `[loop] max_retries` slot, escalates to `loom:blocked` cause
  `retry-exhausted` on exhaustion) so a grep for the rule succeeds
  [check](grep -qi 'LOOM_RETRY' crates/loom-templates/templates/partial/self_report_markers.md)
- The partial body distinguishes `LOOM_BLOCKED` from `LOOM_CLARIFY`:
  blocked = genuine dead end, no candidate resolutions; clarify =
  decision the agent can frame as a structured `## Options — …`
  block. The discriminator (can the agent enumerate options?) is
  named explicitly
  [check](grep -qi 'candidate resolution\|enumerate options' crates/loom-templates/templates/partial/self_report_markers.md)
- The partial body identifies the worker-phase scoping: `LOOM_RETRY`,
  `LOOM_CLARIFY`, `LOOM_BLOCKED` are valid in worker phases (`loop`,
  `todo`, `review`) only; interactive sessions (`plan`, `inbox`) do not emit
  worker self-report markers because the human resolves friction in-turn
  [check](grep -qi 'worker.*phase\|interactive.*session' crates/loom-templates/templates/partial/self_report_markers.md)

### Mint default-profile

- The driver-side `loom gate mint` resolves the per-spec default
  profile via `default_profile_for_spec(&SpecLabel)`; cargo-bound
  specs (`harness`, `templates`, `agent`, `gate`, `llm`, `tests`)
  resolve to `profile:rust`
  [check](cargo test -p loom-workflow --lib default_profile_for_spec_returns_rust_for_cargo_bound_specs)
- Nix-only / unknown specs fall through to `profile:base`
  [check](cargo test -p loom-workflow --lib default_profile_for_spec_returns_base_for_nix_only_specs)
- Mint applies the resolved default profile as a `profile:<name>`
  label on every fix-up and clarify bead it creates; the operator
  overrides via `bd update <id> --labels` post-mint
  [test](mint_applies_per_spec_default_profile_label_to_created_beads)

### Typed `PreviousFailure`

- `PreviousFailure` is a tagged enum with variants `DriverNotice`,
  `VerifyFailures(Vec<VerifierFailure>)`,
  `ReviewConcern { summary, findings }`, `BadWalk(BadWalk)`,
  `BuildFailure`, `TreeNotClean { dirty_paths: Vec<String> }`,
  `PostIntegrateFail { failures }`, and
  `AgentRetry { reason: String }` — not a free string
  [check](grep -q 'pub enum PreviousFailure' crates/loom-templates/src/previous_failure.rs)
- `PreviousFailure::AgentRetry { reason }` variant exists and
  carries the verbatim prose the agent wrote on the line preceding
  the `LOOM_RETRY` marker; populated by the driver when a worker
  phase exits with `LOOM_RETRY` per
  [harness.md § Verdict Gate](harness.md#verdict-gate)
  [check](grep -q 'AgentRetry' crates/loom-templates/src/previous_failure.rs)
- The `Display for PreviousFailure` rendering of `AgentRetry`
  surfaces the agent's prior `reason` and instructs the retry
  attempt to escalate to `LOOM_BLOCKED` or `LOOM_CLARIFY` if the
  same problem persists after retry
  [test](agent_retry_display_renders_reason_and_escalation_guidance)
- `BadWalk` enum carries `Concern { payload: String, parsed_findings: Vec<Finding> }`,
  `ConcernWithoutFindings { summary: String }`,
  `FindingsWithoutConcern { finding_count: usize, findings: Vec<Finding> }`,
  and `MalformedFinding { errors: Vec<FindingParseError>, terminal: TerminalSurface }`;
  the wrapped pattern mirrors `RecoveryCause::ReviewConcern(ReviewFlag)` at the
  type level
  [check](grep -q 'pub enum BadWalk' crates/loom-protocol/src/gate.rs)
- Maximum-context preservation invariant: `BadWalk::Concern` carries
  `parsed_findings` (any well-formed findings streamed ahead of the
  malformed terminator); `BadWalk::FindingsWithoutConcern` carries
  `findings` (the parsed Vec<Finding> the agent emitted); and
  `BadWalk::MalformedFinding` carries the well-formed `terminal`
  alongside the per-line errors. Construction of any variant
  without its max-context fields is a compile error
  [test](bad_walk_variants_preserve_max_context_invariant_by_struct_shape)
- `TerminalSurface` enum mirrors `ExitSignal` with explicit
  `Malformed { payload: String }` and `Missing` variants so
  `BadWalk::MalformedFinding`'s `terminal` field can carry the
  terminal state regardless of whether the terminal itself parsed
  [check](grep -q 'pub enum TerminalSurface' crates/loom-protocol/src/gate.rs)
- `FindingParseError` is defined in `loom-protocol::gate` and
  re-exported from `loom-templates::finding` /
  `loom-workflow::review::finding` as the per-line wire-format error
  consumed by `BadWalk::MalformedFinding.errors`
  [check](grep -q 'pub enum FindingParseError' crates/loom-protocol/src/gate.rs)
- The `Display for PreviousFailure` rendering of
  `BadWalk(Concern)` appends a per-finding digest of
  `parsed_findings` when non-empty (the agent's diagnosis from the
  streamed findings is surfaced even when the terminal was
  malformed)
  [test](bad_walk_concern_display_renders_parsed_findings_digest_when_present)
- The `Display for PreviousFailure` rendering of
  `BadWalk(FindingsWithoutConcern)` appends a per-finding digest of
  `findings` so the agent's next iteration sees the diagnosis it
  just emitted
  [test](bad_walk_findings_without_concern_display_renders_findings_digest)
- The `Display for PreviousFailure` rendering of
  `BadWalk(MalformedFinding)` enumerates per-line errors AND
  surfaces the well-formed `terminal` via its rendered form so the
  agent fixes the fence/format without losing the surrounding
  context
  [test](bad_walk_malformed_finding_display_surfaces_terminal_and_per_line_errors)
- `TreeNotClean` variant carries `dirty_paths: Vec<String>` capped
  at 30 entries by the driver before construction
  [check](grep -q 'TreeNotClean' crates/loom-templates/src/previous_failure.rs)
- `PostIntegrateFail` variant carries `failures: Vec<VerifierFailure>`
  and `gate_log_path: PathBuf` directly; populated when the
  loom-workspace per-bead integration step's verify against the
  integrated tree fails after the bead's own verify passed at its
  bead workspace. Review concerns are not a possible cause — they
  route through `ReviewConcern` / `BadWalk` after verify succeeds.
  [check](grep -q 'gate_log_path' crates/loom-templates/src/previous_failure.rs)
- `DriverNoticeCause` enum covers `SwallowedMarker`,
  `IncompleteSignaling`, `ZeroProgress`, `ObserverAbort`,
  `RetryExhausted`, `UnbondedOrigin`
  [test](driver_notice_cause_labels_match_spec_strings)
- `VerifierFailure` carries `target: String`, `exit_code: i32`,
  `stderr_tail: String` (capped per-block at ~1500 chars)
  [test](verifier_failure_stderr_tail_capped_per_block)
- Total `previous_failure` budget capped at
  `PREVIOUS_FAILURE_MAX_LEN = 4000` chars; multi-block variants
  split budget across entries with later entries truncated first
  [test](verify_failures_split_budget_truncates_later_first)
- `review_notes` field is separate from `previous_failure`, has
  its own ~1000-char budget, and is populated only when
  `previous_failure` is `VerifyFailures` and review also raised a
  concern
  [test](review_notes_populated_only_on_verify_fail_plus_review_concern)
- Each `PreviousFailure` variant renders with its documented
  framing prefix (`DriverNotice` → "Previous attempt:",
  `VerifyFailures` → "Verifier failures from previous attempt:",
  `ReviewConcern` → "Review raised {N} concern(s) — {summary}",
  `BadWalk` → per-variant fragment naming the specific
  malformation, `BuildFailure` → "Build failed at ...:",
  `TreeNotClean` → "Working tree was not clean after the bead
  committed:", `PostIntegrateFail` → "After rebasing onto the
  integration branch, the post-integration verify failed.")
  [test](previous_failure_variant_framings_match_spec)
- `TreeNotClean` renders the dirty-path list one-per-line and
  appends a `"+N more"` suffix line when the upstream driver
  truncated past 30 entries
  [test](tree_not_clean_renders_path_list_with_truncation_suffix)

### Attempt counter

- `LoopContext` carries `attempt: u32`; field is `0` on fresh
  bead dispatch
  [test](attempt_zero_on_fresh_bead_dispatch)
- `loop.md` omits the attempt line when `attempt == 0`
  [test](run_template_omits_attempt_line_when_zero)
- `loop.md` renders "Retry attempt {N} — previous attempt failed
  with: …" when `attempt > 0 && previous_failure.is_some()`
  [test](run_template_renders_attempt_line_on_retry)
- Attempt counter is per-bead in-session: fix-up beads start at
  `attempt = 0` regardless of the failing bead's prior attempts
  [test](fix_up_bead_starts_at_attempt_zero)
- Attempt counter is bounded by `[loop] max_retries` (default 2)
  [test](failed_bead_retries_with_previous_failure_then_clarifies)

### First-instruction reframe

- `loop.md` prepends "Re-read the previous failure block above and
  address its specific concern before re-implementing." when
  `previous_failure.is_some()`
  [test](run_template_prepends_first_instruction_reframe_on_retry)
- Reframe is omitted when `previous_failure.is_none()`
  [test](run_template_omits_first_instruction_reframe_on_fresh_dispatch)
- Reframe wording is generic (one form regardless of variant);
  per-variant detail lives inside the previous-failure block itself
  [check](grep -q 'Re-read the previous failure block above' crates/loom-templates/templates/loop.md)

### Loop completion self-check and self-review

- `loop.md` instructs the worker to run
  `loom gate verify --diff <bead-base>..HEAD` (or `@{u}..HEAD` only
  when upstream is that base) before emitting `LOOM_COMPLETE`; it does
  not name `loom gate verify --diff HEAD` as the final self-check
  [test](run_template_uses_injected_self_check_range_not_head_shorthand)
- `loop.md` tells the worker to rerun the self-check after any later
  commit or hook-generated file change
  [test](run_template_requires_self_check_rerun_after_post_check_changes)
- `loop.md` requires prompt-level self-review before the final marker:
  re-read criteria, inspect the final diff, check style/spec fit, and
  fix issues or emit `LOOM_RETRY` / `LOOM_CLARIFY` / `LOOM_BLOCKED`
  [judge](../tests/judges/loom.sh#judge_loop_self_review_before_complete)

### Public surface

- `templates` exposes `PreviousFailure`, `VerifierFailure`,
  `BadWalk`, `DriverNoticeCause`, `CriterionStatus`,
  `EvidenceState`, `CriterionId`, `CriterionAnnotation`,
  `LoopContext`, `ReviewContext`, `PlanContext`, `TodoContext`, and
  `PinnedContext` as public types consumable from external crates
  [check](cargo run -p loom-walk -- loom_templates_public_types)
- Each partial in the *Partials* table is also exposed as a public
  `&'static str` constant (e.g. `SCRATCHPAD_PARTIAL`,
  `CONTEXT_PINNING_PARTIAL`, etc.) for consumer template composition
  [check](cargo run -p loom-walk -- loom_templates_public_partial_constants)
- Loom's workflow template bodies themselves (`plan.md`, `todo.md`,
  `loop.md`, `review.md`, `inbox.md`) are NOT publicly exported —
  only the typed contexts and partial strings
  [check](cargo run -p loom-walk -- loom_templates_workflow_templates_not_exported)

### Criterion-status surface

- `TodoContext` carries `criterion_status: Vec<CriterionStatus>`; no
  other phase context does
  [check](cargo run -p loom-walk -- todo_contexts_carry_criterion_status)
- `CriterionStatus` is a struct with fields `spec_label`,
  `criterion_id`, `criterion_text`, `annotation`, and `evidence`;
  `EvidenceState` is a tagged enum with variants `Current`, `Missing`,
  and `StaleAnnotation`
  [check](grep -q 'pub struct CriterionStatus' crates/loom-templates/src/criterion_status.rs)
- `todo.md` rendered prompts surface every changed spec's
  `CriterionStatus` rows with criterion text, annotation, and evidence
  state so the agent can distinguish current pass evidence from stale or
  missing evidence
  [test](todo_template_renders_typed_criterion_status_rows)

### Decomposition discipline

- `partial/decomposition_discipline.md` exists and is included by
  `todo.md` only; the body names the exact changed-spec roster, the
  evidence-confirmation obligation, and the `LOOM_TODO` success shape
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- The partial body names the discipline distinctively (so a grep
  catches accidental emptying)
  [check](grep -qi 'exact.*roster\|evidence-confirmed\|LOOM_TODO' crates/loom-templates/templates/partial/decomposition_discipline.md)
- Rendered `todo.md` prompts contain a clause committing the agent to
  confirm missing work by inspection before authoring any non-audit bead
  [test](todo_template_renders_pre_decomposition_audit_clause)
- The partial documents `LOOM_CLARIFY` on the `loom:todo` work epic as
  the fallback when coverage cannot be determined, with the
  `## Options — …` block per [gate.md](gate.md)'s Options Format
  Contract
  [check](grep -q 'LOOM_CLARIFY' crates/loom-templates/templates/partial/decomposition_discipline.md)
- `todo.md` receives an already-created work epic from the driver before
  any path that can emit `LOOM_CLARIFY`
  [check](cargo run -p loom-walk -- todo_template_uses_driver_created_work_epic)

## Requirements

### Functional

1. **Compiled workflow templates.** Every Loom-workflow phase
   prompt (`plan`, `todo`, `loop`, `review`, `inbox`) is an
   Askama template compiled into the binary. Template correctness
   is verified at compile time. No per-project mechanism hot-overrides
   Loom's workflow templates at runtime; `loom tune phase fast|run|full` and
   `loom tune partial fast|run|full` create reviewed source-change proposals instead.
   (Consumers writing their own templates for their own LLM calls via
   `llm` use the public typed building blocks described below; this FR is
   specifically about Loom's own workflow templates.)
2. **One template per phase** as enumerated in *Template Files* above;
   `plan` and `todo` no longer split into new/update modes.
3. **Partials** as enumerated in *Partials* above. Each partial
   declares which templates include it; the matrix in *Pinning
   Policy* is the authoritative listing.
4. **Typed context per template.** Each template has a Rust
   `#[derive(Template)]` struct with one field per variable. The
   variable set is enumerated in *Template Variables*.
5. **Per-phase pinning.** Partial inclusion follows *Pinning Policy*;
   `style_rules.md` is pinned in `loop` and `review` only;
   `spec_conventions.md` is pinned in `plan` only; `todo_success.md` is
   pinned in `todo` only. Matrix cells use the four-value vocabulary
   `✓` / blank / `?` (pending addition) / `~` (pending removal) per
   [gate.md § Pending support in structured walker
   input](gate.md#pending-support-in-structured-walker-input); the
   pinning-matrix walker enforces the assertion at the appropriate scope
   and fails with `pending-marker-resolved` when a pending marker's state
   catches up.
6. **Rule-family agnosticism.** The `style_rules.md` and
   `review_rubric.md` partial bodies discover rule families from
   the pinned `{{ style_rules }}` document. Template bodies do
   not enumerate fixed prefixes.
7. **Agent-output markers.** All agent-generated content rendered
   back into a prompt is wrapped in `<agent-output>` /
   `</agent-output>`.
8. **Skill index.** `partial/skill_index.md` is included by every
   agent-bearing template and renders a `SkillIndexMarkdown` value produced by
   `loom-skills`. It lists compact entries only; full skill bodies remain
   on-demand files or native backend registrations.
9. **Template tuning validation.** `loom tune phase fast|run|full` and `loom tune partial fast|run|full`
   candidates must compile under Askama, render representative snapshots, and
   pass template conformance walkers in the proposal worktree before entering
   `loom inbox`.
10. **Snapshot tests.** Every template × representative-input
   combination has an `insta` snapshot.
11. **Typed `PreviousFailure`** — `LoopContext.previous_failure` is
   `Option<PreviousFailure>` where `PreviousFailure` is a tagged
   enum (`DriverNotice`, `VerifyFailures`, `ReviewConcern`,
   `BadWalk(BadWalk)`, `BuildFailure`, `TreeNotClean`,
   `PostIntegrateFail`, `AgentRetry { reason: String }`). The driver
   populates the right variant from the verdict-gate cause
   classification. Each variant renders with distinct framing per
   *Typed `PreviousFailure`* above. Caps:
   `PREVIOUS_FAILURE_MAX_LEN = 4000` total; per-block stderr tail
   ~1500 chars; `review_notes` separate ~1000-char budget.
   `AgentRetry.reason` shares the per-block budget cap.
12. **Attempt counter.** `LoopContext.attempt: u32` is the per-bead
    in-session retry counter, bounded by `[loop] max_retries`
    (default 2), resets to 0 on fresh bead dispatch. Fix-up beads
    start at `attempt = 0`; work-epic-level iteration is opaque to
    the agent. `loop.md` renders the attempt line when `attempt > 0
    && previous_failure.is_some()`, omits it otherwise.
13. **First-instruction reframe.** When
    `previous_failure.is_some()`, `loop.md` prepends "Re-read the
    previous failure block above and address its specific concern
    before re-implementing." Single generic form — per-variant
    detail lives in the previous-failure block itself.
14. **Public surface for consumers.** `templates` is a
    public-contract crate. Exposed: `PreviousFailure` (and its
    sub-types), `CriterionStatus`, `EvidenceState`, `CriterionId`,
    `CriterionAnnotation`, `SkillIndexMarkdown`, `PlanContext`, `TodoContext`,
    `LoopContext`, `ReviewContext`, `PinnedContext`, and the partial-string
    constants for each entry in the *Partials* table. Loom's workflow template
    bodies themselves are not exposed — consumers compose their own
    templates from the typed contexts + partial strings, not from Loom's
    workflow templates. Stability: additive type changes are minor bumps;
    removing or renaming fields / partial paths is a major bump.

    **Dependency on `loom-protocol`.** The typed gate wire-format
    contract (`Finding`, `ConcernToken`, `FindingTarget`, `BadWalk`,
    `WalkOutput`, etc.) lives in `loom-protocol::gate` — see
    [gate.md § Canonical contract location](gate.md#canonical-contract-location).
    The typed todo success contract (`TodoSuccess`, `TodoSpecSuccess`,
    `TodoSpecOutcome`, `TodoFingerprint`) lives in
    `loom-protocol::todo` per [harness.md](harness.md). `loom-templates`
    depends on `loom-protocol` so
    `PreviousFailure::ReviewConcern { findings: Vec<Finding> }` and
    `PreviousFailure::BadWalk(BadWalk)` can carry the typed values;
    `loom-templates` re-exports the gate contract via `pub use` so
    existing consumers importing from `loom-templates::finding`
    continue to compile. The intended consumption shape for a
    consumer writing their own LLM pipeline against loom: depend on
    `loom-protocol` (parse `loom gate ...` subprocess stdout into
    typed `WalkOutput`), depend on `loom-templates` (compose their
    own Askama template body that `{% include %}`s `PARTIAL_*`
    constants and fills typed contexts), depend on `loom-llm`
    (run the conversation loop). The three crates compose; loom CLI
    is itself one such consumer.

    **Dogfood is structural.** Loom CLI uses the same Askama
    mechanism, the same exposed partials, and the same typed
    contexts a consumer would use — there is no "loom's special
    path" vs "consumer's path." Loom's CLI binary depends on
    `loom-templates` exactly like a consumer would. The boundary
    that keeps consumers from forking loom's workflow bodies is
    the deliberate non-exposure of those bodies (the "Loom's
    workflow template bodies themselves are not exposed" rule in the public
    surface requirement), not a divergent loading mechanism.

    `PARTIAL_FINDINGS_WALK` is the canonical agent-facing prose for
    the gate wire format and is paired with `loom-protocol::gate` on
    the parser side. Consumers using `loom-protocol::gate::parse_walk_output`
    to parse subprocess stdout should pair it with `PARTIAL_FINDINGS_WALK`
    in their own template body so the emitter (their LLM agent) and
    the parser (their driver) stay coherent across loom releases. The
    anti-drift coupling between `ConcernToken` and `PARTIAL_FINDINGS_WALK`
    is maintained inside loom's workspace by the
    `template_wire_format_restatement` walk; consumers get coherence
    for free as long as they pin both crates from the same loom
    release.
15. **Chat discipline in interactive sessions.**
    `partial/chat_interview.md`, pinned in every interactive-session
    template (`plan`, `inbox`), requires the interactive agent to conduct
    conversations as back-and-forth prose and forbids Claude Code's
    structured option-picker tool (`AskUserQuestion` or any equivalent
    multi-choice widget).
    Options are listed inline in prose; the user replies in prose.
    The partial also carries the **persistence-destination clause**:
    session-bridging memory (decisions, context, follow-ups) goes
    only to the durable surface the phase authorizes: `loom plan`
    writes spec/index markdown or implementation notes and does not
    write bd, while `loom inbox` can use bd notes/descriptions for
    resolutions. Claude Code's `MEMORY.md` system is container-local
    and disappears with the container. The "one by one" sub-mode is
    planning-specific and lives in a separate partial; the chat-
    discipline rules above apply to every interactive session,
    including inbox-chat.
16. **Criterion-status surface for decomposition.** `TodoContext`
    carries `criterion_status: Vec<CriterionStatus>` where each row
    exposes `spec_label`, typed `criterion_id`, criterion text, typed
    annotation, and `EvidenceState` (`Current`, `Missing`,
    `StaleAnnotation`). The driver populates the surface by parsing the
    changed specs and joining against `.loom/cache.db`'s criterion
    evidence cache. Missing cache rows become `EvidenceState::Missing`,
    never no work. The struct does not encode staleness thresholds — the
    partial body owns the heuristic.
17. **Decomposition discipline in `todo`.**
    `partial/decomposition_discipline.md`, pinned in `todo` only,
    requires the decomposition agent to decompose the driver-injected
    changed-spec roster exactly, confirm missing work by consulting
    `criterion_status` and representative implementations before
    authoring non-audit beads, create beads only under the injected
    `loom:todo` work epic, and use `LOOM_TODO: <json>` as the only
    success marker. `LOOM_CLARIFY` targets the work epic with a
    `## Options — …` block when coverage cannot be determined.
18. **Self-report marker taxonomy.** The worker-phase self-report
    markers form a three-way taxonomy carried by
    `partial/self_report_markers.md`:
    - `LOOM_RETRY` — this attempt cannot finish but a fresh dispatch
      is likely to succeed (environmental failure: tools failing
      mid-session, sandbox/cwd unlinked, transient IO; or agent
      self-reset: stuck-but-not-blocked, prompt-context exhausted).
      Consumes one slot in `[loop] max_retries`; exhaustion escalates
      to `loom:blocked` with cause `retry-exhausted` per
      [harness.md § Verdict Gate](harness.md#verdict-gate). The
      driver populates `PreviousFailure::AgentRetry { reason }`
      with the prose the agent wrote on the line preceding the
      marker.
    - `LOOM_CLARIFY` — the agent has framed a decision the human
      must resolve and can enumerate the candidate paths as a
      structured `## Options — …` block per
      [gate.md § Options Format Contract](gate.md#options-format-contract).
      Routes to `loom:clarify` for human resolution via `loom inbox`.
    - `LOOM_BLOCKED` — genuine dead end: the agent cannot proceed
      and has no candidate resolutions to enumerate. Routes to
      `loom:blocked`; `loom inbox chat` walks the human through
      candidate enumeration in-session.

    The semantic discriminator between the three is explicit and
    grep-able in the partial body: "expect retry to succeed? →
    RETRY. can you enumerate options? → CLARIFY. dead end? → BLOCKED."
    The taxonomy applies to worker phases only (`loop`, `todo`,
    `review`); interactive sessions (`plan`, `inbox`) do not emit worker
    self-report markers — the human resolves friction in-turn. `inbox` may emit
    `LOOM_APPLY: {"proposals":[...]}` when it requests the trusted driver to
    apply accepted tune proposals.
19. **Options-block requirement on clarify-bound findings.**
    `partial/findings_walk.md` requires every clarify-bound finding
    (any token whose mint would label the resulting bead
    `loom:clarify`, not only `invariant-clash`) to embed the
    canonical `## Options — <summary>` block (with at least one
    `### Option <N> — <title>` subsection) inside its `evidence`
    payload. The driver-side `loom gate mint` validates the evidence
    at parse time; clarify-bound findings whose evidence lacks a
    well-formed options block fall back to `loom:blocked` with
    cause `clarify-without-options` per
    [gate.md § Options Format Contract](gate.md#options-format-contract).
    No wire-format extension to the `LOOM_FINDING:` JSON payload —
    the contract lives in the `evidence` field's content, with the
    enforcement at the mint chokepoint. The agent should emit
    `LOOM_BLOCKED` directly when it cannot articulate options
    rather than emitting a clarify-bound finding without them.

### Non-Functional

1. **Compile-time validation.** Template syntax errors, undefined
   variables, and missing partial files all fail the build, not
   discovered at runtime.
2. **Style.** Follows the team's
   [`docs/style-rules.md`](../docs/style-rules.md).

## Out of Scope

- **Spec-lifecycle CLI commands.** Splitting, merging, renaming,
  and superseding specs are decisions made inside a planning
  session, with judgment applied to which sections move, which
  beads reassign, and which cross-refs rewrite. The CLI exposes
  no dedicated split / merge / rename / supersede commands.
- **Runtime override of Loom's workflow templates.** Loom's `plan` /
  `todo` / `loop` / `review` / `inbox` templates are Askama, compiled into the
  binary. `loom tune phase fast|run|full` / `loom tune partial fast|run|full` may propose source edits in an
  isolated worktree, but there is no per-project template-fetch or runtime
  template override for Loom's own templates. Project-specific prompt tweaks to
  Loom's workflow happen via `pinned_context`, `style_rules`,
  `spec_conventions`, skills, and per-spec implementation notes. Consumers
  writing their *own* templates (for their own LLM calls via `llm`) compose them
  from the exposed typed building blocks (above) — that path is supported and is
  *not* what this exclusion covers.
- **Runtime template engine for consumer overrides of Loom's
  workflow templates.** Adding a runtime engine (e.g. `minijinja`)
  to allow consumers to drop in replacements for Loom's compiled
  Askama templates is bolt-on-able after the typed-context public
  surface lands and is deferred until a concrete consumer asks.
- **Untyped `previous_failure`.** `LoopContext.previous_failure` is
  `Option<PreviousFailure>` — a typed enum, not a free string.
  Free-string detail (driver formats prose into a String the
  template prints unchanged) is excluded so heading shape, caps,
  and multi-cause composition stay owned by the typed contract
  rather than re-derived at every emit site.
- **Template content changes.** The *rules* themselves live in
  `docs/style-rules.md`; this spec only pins the file and does not
  own its content. The *conventions* themselves live in
  `docs/spec-conventions.md` similarly.
- **Selective rule filtering in the pin.** The
  `partial/style_rules.md` pin points at the whole document;
  agents read the families relevant to their work. Revisit if
  prompt-size measurements show the unselected pin is materially
  expensive.

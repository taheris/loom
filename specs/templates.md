# Loom Templates

Askama template engine, partials inventory, per-phase pinning
policy, snapshot-test contract, and public-contract typed building
blocks consumers compose into their own templates.

## Problem Statement

Loom's agent-bearing workflow phase prompts (`plan`, `todo`, `loop`,
`review`, `msg`) are rendered from Askama templates compiled into
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

One template per phase, plus per-mode variants:

- `plan_new.md`, `plan_update.md`
- `todo_new.md`, `todo_update.md`
- `loop.md`, `review.md`, `msg.md`

`loom gate verify` is deterministic — it runs verifiers, audits,
and linters without rendering any agent prompt — so it has no
template. `loom gate review` is the LLM-judged counterpart and
has its own template, distinct from `loop.md` because the review
session has different inputs (diff, bead intent, sibling diffs,
prior `loom gate verify` results) and a rubric-walk objective
rather than an implement-the-bead objective.

Each template has a matching `#[derive(Template)]` context struct
in the same crate. The Askama build verifies every variable
referenced in the template body has a matching field on its
context struct — missing variables are compile errors, unused
fields trigger the `unused` workspace lint.

### Partials

Reusable fragments included via `{% include "partial/<name>.md" %}`.
Current set:

| Partial | Purpose |
|---------|---------|
| `context_pinning.md` | Pin the project-overview file (`pinned_context`) |
| `style_rules.md` | Pin the style-rules file (`style_rules`) — see *Style-Rules Partial* below |
| `spec_conventions.md` | Pin the spec-conventions document — see *Spec-Conventions Partial* below |
| `spec_header.md` | Render spec label, path, active molecule |
| `companions_context.md` | List companion paths declared on the spec |
| `scratchpad.md` | Pin the per-session scratchpad path |
| `progress_markers.md` | Document the `LOOM_COMPLETE` / `LOOM_NOOP` "work is done" terminators. **Markers are mutually exclusive — exactly one per session, on the final line.** Pinned in every phase (interactive sessions emit `LOOM_COMPLETE`; worker phases emit either `LOOM_COMPLETE` or `LOOM_NOOP`); paired with `self_report_markers.md` in worker phases so each worker phase surfaces both the success and the cannot-finish terminator sets. |
| `self_report_markers.md` | Document the worker-phase cannot-finish terminators `LOOM_RETRY`, `LOOM_CLARIFY`, `LOOM_BLOCKED`. Each routes a distinct outcome: retry on transient failure (environmental or agent self-reset), clarify on a decision the agent can frame as `## Options — …`, blocked on a genuine dead end with no candidate resolutions. **Pinned in worker phases only** (`todo_*`, `loop`, `review`) — interactive templates (`plan_*`, `msg`) emit `LOOM_COMPLETE` only because the human is present and resolves friction in-turn, so the cannot-finish markers are out of scope for those templates. The `chat_marker_final_turn_only.md` partial reinforces the COMPLETE-only restriction for interactive sessions. |
| `findings_walk.md` | Sole carrier of the `LOOM_FINDING:` / `LOOM_CONCERN:` colon-suffixed wire format (streaming finding lines + JSON-payload terminator + pairing rule) per [gate.md § Findings and Minting](gate.md#findings-and-minting). Documents the **Options-block requirement**: every clarify-bound finding (any finding whose mint would apply `loom:clarify` to the resulting bead, not only `invariant-clash`) MUST embed a canonical `## Options — …` block inside its `evidence` payload. The driver-side mint validates the evidence at parse time and falls back to `loom:blocked` with cause `clarify-without-options` when the block is absent or malformed, per [gate.md § Options Format Contract](gate.md#options-format-contract). Pinned only by `review.md`; an anti-drift `[check]`-tier verifier fails any other template that restates the wire format. |
| `chat_marker_final_turn_only.md` | Restrict `LOOM_COMPLETE` emission to the **final** assistant turn of an interactive session. Included by `msg`, `plan_new`, and `plan_update` to disambiguate `progress_markers.md`'s "end your response with the marker" language (which is correct for single-shot worker phases but misreads as "every response" in chat). One-shot worker phases (`loop`, `todo_*`, `review`) deliberately omit it because every response in those phases IS the final output. |
| `interview_modes.md` | Describe the "one by one" / "polish the spec" interview sub-modes |
| `chat_interview.md` | Interactive-session discipline pinned by every interactive-session template (`plan_new`, `plan_update`, `msg`): conversational prose Q&A only, no Claude Code option-picker / `AskUserQuestion` widget, and bd is the durable persistence destination for anything that needs to outlive the session — see *Chat Discipline* below |
| `decomposition_discipline.md` | Pin the audit-before-fan-out rule on `todo_new` / `todo_update`: every bead must correspond to evidence-confirmed missing work, not a spec criterion in the abstract — see *Decomposition Discipline* below |
| `plan_stage_rubric.md` | Gate the planning interview on completeness / coherence / invariant-clash before any commit. Carries the **pending-modifier discipline** prominently — see *Planning-Rubric Pending Discipline* below for what the partial body must spell out, including the "modified annotations" case the rule must explicitly cover (not only newly-added ones). |
| `invariant_clash.md` | Describe the invariant-clash awareness scan (included transitively via `plan_stage_rubric.md`) |
| `review_rubric.md` | Per-diff review rubric — see [gate.md](gate.md) |
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
addition — will resolve to `✓` once the template's `{% include %}`
graph catches up), `~` (pending removal — will resolve to blank
once the template's `{% include %}` is dropped). Pending cells
silent-pass during the pending window per [gate.md § Pending
support in structured walker input](gate.md#pending-support-in-structured-walker-input);
the walker fails — with a `pending-marker-resolved` finding —
once the actual include state catches up to the pending direction
so the author drops the marker to its resolved value (`✓` or
blank) in the same diff.

| Partial | `plan_new` | `plan_update` | `todo_new` | `todo_update` | `loop` | `review` | `msg` |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| `context_pinning.md` | ✓ | ✓ | ✓ | ✓ | ? | ✓ | ✓ |
| `style_rules.md` |  |  |  |  | ✓ | ✓ |  |
| `spec_conventions.md` | ✓ | ✓ |  |  |  |  |  |
| `spec_header.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |  |
| `companions_context.md` |  | ✓ | ✓ | ✓ | ? | ✓ | ✓ |
| `scratchpad.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `progress_markers.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |  |
| `self_report_markers.md` | ~ | ~ | ✓ | ✓ | ✓ | ✓ |  |
| `findings_walk.md` |  |  |  |  |  | ✓ |  |
| `chat_marker_final_turn_only.md` | ✓ | ✓ |  |  |  |  | ✓ |
| `interview_modes.md` | ✓ | ✓ |  |  |  |  |  |
| `chat_interview.md` | ✓ | ✓ |  |  |  |  | ? |
| `decomposition_discipline.md` |  |  | ✓ | ✓ |  |  |  |
| `plan_stage_rubric.md` | ✓ | ✓ |  |  |  |  |  |
| `invariant_clash.md` | ✓ | ✓ |  |  |  |  |  |
| `review_rubric.md` |  |  |  |  |  | ✓ |  |
| `sibling_spec_editing.md` |  | ✓ |  |  |  |  |  |

The pending cells reflect planning-session edits whose
template-wiring implementation hasn't landed yet:
`self_report_markers.md` is `~` for `plan_new` / `plan_update`
(pending removal — the template currently includes the partial;
the include must be dropped per the worker-only marker
discipline); `chat_interview.md` is `?` for `msg` (pending
addition — the template doesn't yet include the partial; the
include must be added per the interactive-session discipline
pinning); `companions_context.md` and `context_pinning.md` are
`?` for `loop` (pre-existing drift — the templates don't include
these partials but the spec asserts they should; the resolving
session either adds the `{% include %}` to `loop.md` and drops
the `?` to `✓`, or drops the assertion entirely by changing the
cell to blank). Each pending marker resolves to `✓` or blank in
the same diff that lands the include change, enforced by the
walker's `pending-marker-resolved` finding when state catches up.

**`style_rules.md` is pinned only in `loop` and `review`** — the two
phases that write or evaluate code (`loop` produces it, `review`
judges it). Other phases (planning, decomposition, clarify
resolution) don't write or evaluate code, so pinning the rules
there would inflate prompt size without buying enforcement.

**`spec_conventions.md` is pinned only in `plan_new` and
`plan_update`** — the two phases that author spec content. Other
phases consume specs but don't modify them.

**`decomposition_discipline.md` is pinned only in `todo_new` and
`todo_update`** — the two phases that authorize bead creation.
The discipline is decomposition-specific: it tells the agent to
confirm missing work by inspection before authoring any non-audit
bead, and to fall back to `LOOM_CLARIFY` on the molecule epic when
coverage cannot be determined. See *Decomposition Discipline* for
the invariant and the two acceptable session outcomes.

### Template Variables

Each variable is bound to a typed field on the relevant context
struct. `String`-typed values arriving from beads or config flow
through the parse-don't-validate boundary defined in
[harness.md](harness.md#parse-dont-validate).

| Variable | Type | Used By |
|----------|------|---------|
| `pinned_context` | `String` | all |
| `style_rules` | `String` | `loop`, `review` |
| `spec_conventions` | `String` | `plan_new`, `plan_update` |
| `label` | `SpecLabel` | all |
| `spec_diff` | `Option<String>` | `todo_update` |
| `existing_tasks` | `Option<String>` | `todo_update` |
| `companion_paths` | `Vec<String>` | `plan_update`, `todo_*`, `loop`, `review`, `msg` |
| `clarify_beads` | `Vec<ClarifyBead>` | `msg` |
| `implementation_notes` | `Vec<String>` | `todo_new`, `todo_update` |
| `molecule_id` | `Option<MoleculeId>` | `todo_update`, `loop` |
| `issue_id` | `Option<BeadId>` | `loop` |
| `title` | `Option<String>` | `loop` |
| `description` | `Option<String>` | `loop` |
| `previous_failure` | `Option<PreviousFailure>` | `loop` (retry only; typed enum — see *Typed `PreviousFailure`* below) |
| `review_notes` | `Option<String>` | `loop` (set only when `previous_failure` is `VerifyFailures` and review also raised a concern) |
| `attempt` | `u32` | `loop` (in-session per-bead retry counter — see *Attempt Counter* below) |
| `beads_summary` | `Option<String>` | `review` |
| `base_commit` | `Option<String>` | `review` |
| `criterion_status` | `Vec<CriterionStatus>` | `todo_new`, `todo_update` (see *Criterion-Status Surface* below) |
| `scratchpad_path` | `String` | all |

The newtypes (`SpecLabel`, `MoleculeId`, `BeadId`) are
architecture-bearing types defined in
[harness.md](harness.md#parse-dont-validate); the
template treats them as opaque typed values.

`implementation_notes` is sourced from the state DB's `notes` table
(kind = `implementation`); see *Notes lifecycle* in
[harness.md](harness.md#sqlite-state-store).

### Criterion-Status Surface

`criterion_status` is the per-criterion record that gives `todo_*`
decomposition agents evidence of which Success-Criteria bullets
already pass before they fan out beads. Without this surface, the
agent has only the spec text and a directory listing — the input
shape that drives a decomposition agent to author beads for spec
criteria whose verifiers already pass.

```rust
pub struct CriterionStatus {
    /// Stable identifier for this criterion within the spec
    /// (e.g. the trailing fragment of its anchor in the rendered
    /// markdown). Format owned by the gate's status cache.
    pub criterion_anchor: String,

    /// The annotation target as the criterion declared it
    /// (`[check](...)`, `[test](...)`, `[system](...)`, `[judge](...)`).
    pub annotation: String,

    /// Last cached verdict for this criterion's verifier.
    pub last_result: CriterionResult,

    /// Unix-millis timestamp of the verifier run that produced
    /// `last_result`. `None` if no run has ever populated the cache.
    pub last_timestamp_ms: Option<i64>,

    /// Commit hash the cached result was recorded against. `None`
    /// when `last_result` is `NoResult`.
    pub last_commit: Option<String>,

    /// Number of commits between `last_commit` and the current
    /// HEAD (computed by the driver from `git rev-list --count`).
    /// `None` when `last_commit` is `None`.
    pub commits_since: Option<u32>,
}

pub enum CriterionResult {
    Pass,
    Fail,
    /// The verifier reported the criterion was out of scope for
    /// the run (e.g. file-scoped `--files` filter excluded it).
    Skipped,
    /// No cached run exists — this criterion has never been
    /// verified on this machine.
    NoResult,
}
```

**Source.** The driver constructs `criterion_status` by reading
the status cache that `loom gate verify` / `loom gate review`
populate; cache contents per criterion (annotation target,
last-run timestamp + commit hash, verdict, evidence string) are
owned by [gate.md — Status cache](gate.md#status-cache). The
driver computes `commits_since` against the current HEAD at
prompt-render time. No new schema in gate.md is required for this
surface — the existing cache fields suffice.

**Spec the surface, not the policy.** This struct deliberately
does not encode staleness thresholds (e.g. "≥ 24h is stale"). The
partial body in `partial/decomposition_discipline.md` carries the
heuristic the agent applies to these values; that heuristic can
evolve without spec churn. The struct's contract is just "expose
result + recency"; the agent's prompt instructions decide what
counts as a gap.

**Empty cache.** When no run has populated the cache (fresh
checkout, never-verified spec), every criterion arrives with
`last_result = NoResult`. The partial body treats this as the
strongest signal toward either authoring beads or, when the
volume is too large to inline-audit, emitting `LOOM_CLARIFY`
against the molecule epic.

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

    /// Bead-workspace verify passed, but the loom-workspace per-bead
    /// integration step's `loom gate verify` against the integrated
    /// tree failed (cross-bead interaction, rebase-induced breakage,
    /// integration-tree state no bead-workspace verify could
    /// anticipate). The integration was rolled back via
    /// `git reset --hard HEAD~1`. Carries the verifier-failure list
    /// directly; the per-bead step does not run `loom gate review`,
    /// so review concerns are not a possible cause here (they fire
    /// at the molecule-completion push gate per
    /// [harness.md § Verdict Gate](harness.md#verdict-gate), which
    /// routes through `GateFailReason`, not `PreviousFailure`).
    PostIntegrateFail { failures: Vec<VerifierFailure> },

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
- `BadWalk(MalformedFinding { errors, terminal })` → `"One or more LOOM_FINDING: lines failed parse:\n{per-line: 'Line N: <reason> — raw: <line text>'}\n\nYour terminal was: {terminal-rendered}"`. The terminal rendering uses the typed `TerminalSurface` variant: `Complete` → `"LOOM_COMPLETE"`, `Concern { summary }` → `"LOOM_CONCERN: {summary}"`, `Malformed { payload }` → `"LOOM_CONCERN: <malformed: {payload}>"`, `Missing` → `"(no terminal on the final non-empty line)"`. Surfacing both pieces lets the agent fix the malformed lines (typically: drop the surrounding markdown fence) without losing the well-formed context.
- `BuildFailure` → `"Build failed at {stage}:\n{output}"`
- `TreeNotClean` → `"Working tree was not clean after the bead committed:\n\n{path list, one per line}\n\nStage these into a follow-up commit or revert them."` with a `"+N more"` suffix line when the list is truncated to 30 entries
- `PostIntegrateFail { failures }` → `"After rebasing onto the integration branch, the post-integration verify failed:\n\n{N blocks: target + exit + stderr}\n\nReconcile the cross-bead interaction — your bead's verify passed at its own workspace; the failure is in the integrated tree."`
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

### Agent-Output Markers

Agent-generated content rendered back into a prompt
(`previous_failure`, `title`, `description`, `existing_tasks`) is
delimited with `<agent-output>` / `</agent-output>` markers so the
receiving agent can distinguish injected content from system
instructions. This is a best-effort prompt-injection mitigation;
the real trust boundary is the container.

### Chat Discipline

`partial/chat_interview.md` is included by every interactive-session
template: `plan_new.md`, `plan_update.md`, and `msg.md`. It carries
the discipline shared across every interactive session the loom
binary runs with a human in the loop:

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
  context, follow-ups, anything future sessions need — goes into bd
  (`bd update <id> --notes …`, bead descriptions, or new beads via
  `bd create`) or spec files. bd persists across machines and after
  containers exit. Claude Code's `MEMORY.md` / auto-memory system is
  container-local and disappears with the container; treat it as
  working notes for the current session only, not as durable storage.
- The "one by one" sub-mode (see *Interview Modes*) is planning-
  specific and lives in a separate partial; the chat-discipline rules
  above apply to every interactive session, including msg-chat.

Worker phases (`loop`, `todo_*`, `review`) are single-shot and do not
interview the user, so the partial is not pinned there.

### Decomposition Discipline

`partial/decomposition_discipline.md` is included by `todo_new.md`
and `todo_update.md`. It tells the decomposition agent that every
bead it authors must correspond to **evidence-confirmed missing
work**, not to a spec criterion in the abstract.

Before authoring any non-audit bead, the agent must:

1. Consult the `criterion_status` surface (see *Criterion-Status
   Surface*) for the cached verdict + timestamp + commits-since on
   each criterion in scope. A criterion whose verifier passed in a
   fresh run, with no intervening commits, is positive evidence of
   coverage — not a gap.
2. Read representative existing implementations and verifier
   functions for criteria where `criterion_status` is suspicious
   (stale timestamp, many commits since, or the agent judges the
   verifier name resolves to a path that doesn't exercise the
   live system per [spec-conventions.md](../docs/spec-conventions.md)'s
   "no tier-skipping" rule). A directory listing proves a file
   exists; it does not prove the file contains the named target.

A `loom todo` session has exactly two acceptable outcomes:

- **(a) Gap-targeted bead set.** Beads are authored only for
  criteria the audit confirms are missing, incomplete, or covered
  by a dishonest verifier. The agent cites its evidence (the
  `criterion_status` row, the file read that surfaced the gap, or
  the verifier-source observation) in the bead description.
- **(b) Clarify on the molecule epic.** When coverage cannot be
  determined by inspection — spec ambiguity, conflicting verifier
  targets, or the agent's judgement of cache trustworthiness is
  contestable — the agent emits `LOOM_CLARIFY` with the question
  and `## Options — …` block persisted to the **molecule epic's**
  notes per the *Options Format Contract* in [gate.md](gate.md).
  The verdict gate applies `loom:clarify` to the epic; the human
  resolves via `loom msg <epic>`, and a subsequent `loom todo`
  invocation consumes the answer from the epic's notes before
  fanning out.

Per-bead `loom:clarify` is not appropriate here because the child
beads either don't yet exist (`todo_new`) or are exactly the set
under negotiation (`todo_update`). The epic is the only
session-stable carrier for "this molecule's decomposition is
paused pending clarification".

**Epic-first-always in `todo_new`.** For the clarify-on-epic
fallback to be viable mid-decomposition, the `todo_new.md` flow
creates the molecule epic before any criterion-by-criterion gap
analysis runs. `todo_update` already operates against an
existing molecule, so the ordering is automatic there.

**Enumerate-everything defaults are forbidden by data, not by
grep.** A fixed decomposition axis — e.g. "setup,
implementation, tests, documentation" applied across the board
irrespective of evidence — is the failure mode this discipline
targets. The combined effect of (i) `criterion_status` exposing
positive evidence that whole axes already pass and (ii) the
audit clause's evidence-confirmation prerequisite for bead
authorship makes such fan-outs structurally unviable.
`loom gate review`'s judge-tier walk is what catches any
decomposition that bypasses the `criterion_status` surface to
re-introduce enumerate-everything beads.

**Template-agnostic.** The partial describes the audit obligation
in terms of "criteria in scope" and "representative
implementations", not specific file paths or crate names.
Downstream consumers of loom whose workspace layouts differ from
this one inherit the same discipline against their own layouts.

### Review Emit Shape

`review.md` is the LLM-rubric walk's prompt template. The reviewing
agent emits findings as streaming `LOOM_FINDING:` lines on stdout —
one line per finding, identified as the walk proceeds, with a JSON
payload after the prefix:

```
LOOM_FINDING: {"token": "...", "bonds": ["..."], "target": {"kind": "...", ...}, "evidence": "..."}
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

The `bonds` array on each `LOOM_FINDING:` names the spec(s) the
fix-up should bond to (bonding info); the `target` carries
identity-bearing fields specific to the variant. JSON was chosen
over pipe-delimited shapes because LLM emit is more reliable on
JSON, and the tagged-union encoding of `target` is naturally
JSON-shaped. The terminator's `summary` is a verdict-log entry
only — per-finding routing is decided by `loom gate mint` on the
streamed lines, not on the terminal token.

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

`partial/plan_stage_rubric.md` is pinned in `plan_new.md` and
`plan_update.md`. It owns the planning interview's pre-commit
gate (completeness / coherence / invariant-clash) **and** the
pending-modifier discipline that determines whether the planning
session's spec edits can pass the push gate.

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

`partial/sibling_spec_editing.md` is included only in
`plan_update.md`. It tells the planning agent:

1. The label named on `loom plan -u` is the **anchor**; it owns
   the session state row.
2. During this session, the agent may read and edit any spec in
   `specs/` when a change cross-cuts sibling specs. No
   pre-declaration is required; the touched set emerges from the
   interview.
3. **Creating a new sibling spec is a valid outcome** when the
   planner judges that a section warrants its own spec. The
   planner may allocate a tracking epic for the new sibling and
   record its index entry. This is the one carve-out from the
   general "no bead creation during planning" rule —
   implementation beads for the new spec are created later by
   `loom todo`.
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
- `CriterionStatus`, `CriterionResult` (the decomposition-phase
  criterion-recency surface; consumers writing decomposition-
  style tools reuse this shape against their own caches)
- `LoopContext`, `ReviewContext` (workflow-phase context shapes
  consumers can either reuse directly or model their own contexts
  after)

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

# Spec-authoring conventions — pinned in plan_new and plan_update
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
  `loom gate <X>`. Drift breaks every plan / todo / loop / msg /
  review session by directing the agent at non-existent dispatch
  (Invariant 3 from [gate.md](gate.md))
  [check](cargo run -p loom-walk -- templates_no_removed_surface)

### Pinning policy

- `style_rules.md` partial renders the `style_rules` variable
  [check](grep -q '{{ style_rules' crates/loom-templates/templates/partial/style_rules.md)
- `loop.md` and `review.md` include `style_rules.md`; no other
  phase template does
  [check?](cargo run -p loom-walk -- template_pinning_matrix)
- `spec_conventions.md` partial renders the `spec_conventions`
  variable; included only by `plan_new` and `plan_update`
  [check?](cargo run -p loom-walk -- template_pinning_matrix)
- `LoopContext` and `ReviewContext` carry `style_rules: String`;
  other phase contexts do not
  [check](cargo test -p loom-templates --test render template_renders_are_byte_stable_across_runs)
- `PlanNewContext` and `PlanUpdateContext` carry
  `spec_conventions: String`; other phase contexts do not
  [check](cargo test -p loom-templates --test render template_renders_are_byte_stable_across_runs)
- `LoomConfig.style_rules` defaults to `"docs/style-rules.md"`;
  `LoomConfig.spec_conventions` defaults to
  `"docs/spec-conventions.md"`; `LoomConfig.pinned_context`
  defaults to `"docs/README.md"`
  [test](pin_paths_default_to_bundled_docs)
- Empty string values for any pin path are rejected at parse time
  with `ConfigError::EmptyPath { field }` naming the offending
  field
  [test](empty_pin_path_returns_empty_path_error)
- The `style_rules.md` and `review_rubric.md` partials are
  rule-family-agnostic: their bodies do not enumerate fixed
  prefixes like `SH-` / `RS-` / `COM-`; rule-ID examples in
  template prose are placeholders, not normative
  [check](cargo test -p loom-templates --test render review_renders_style_rule_conformance_walkthrough)
- Every non-pending cell of the pinning matrix above matches the
  actual `{% include %}` graph in `loom-templates/templates/`
  (transitive resolution); drift in either direction — `✓` with no
  include or include with no `✓` — fails the audit. Pending cells
  (`?` and `~`) silent-pass during the pending window per
  *Pinning matrix walker pending support* below
  [check?](cargo run -p loom-walk -- template_pinning_matrix)
- The `chat_marker_final_turn_only.md` partial is included by
  every interactive-session template (`msg`, `plan_new`,
  `plan_update`), pinning the "emit `LOOM_COMPLETE` on the final
  turn only" rule that disambiguates `progress_markers.md`'s
  single-shot "end your response with the marker" wording
  [test](every_multi_turn_template_includes_chat_marker_partial)
- One-shot worker templates (`loop`, `todo_new`, `todo_update`,
  `review`) deliberately **omit** `chat_marker_final_turn_only.md`
  because every response in those phases is the session's final
  output; including the chat-only clause would mislead the agent
  into withholding the marker
  [test](worker_templates_omit_chat_final_turn_clause)
- `partial/chat_interview.md` exists and is included by every
  interactive-session template — `plan_new.md`, `plan_update.md`,
  and `msg.md` — and by no worker template; the body forbids
  Claude Code's structured option-picker tool for interactive Q&A
  and requires conversational prose instead
  [check?](cargo run -p loom-walk -- template_pinning_matrix)
- The partial body names the picker prohibition explicitly so a
  grep for the rule succeeds (no rule-by-implication)
  [check](grep -qi 'option-picker\|AskUserQuestion' crates/loom-templates/templates/partial/chat_interview.md)
- The partial body names the bd-persistence clause distinctively so
  a grep for the rule succeeds: interactive sessions persist
  cross-session memory via bd (notes, descriptions, new beads), not
  via Claude Code's `MEMORY.md` system which is container-local
  [check?](grep -qi 'MEMORY.md\|bd update.*--notes' crates/loom-templates/templates/partial/chat_interview.md)
- `msg.md` rendered prompt contains the chat-interview discipline
  clauses (picker prohibition + bd-persistence) sourced from the
  pinned partial
  [test?](msg_template_renders_chat_interview_discipline)

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
  sibling spec is a valid planning-session outcome and names the
  bead-allocation carve-out
  [judge](../tests/judges/loom.sh#judge_sibling_spec_editing_documents_split)

### Pinning matrix walker pending support

- The pinning-matrix walker accepts `?` (pending addition) and
  `~` (pending removal) as valid cell values in the matrix
  alongside `✓` and blank, per [gate.md § Pending support in
  structured walker input](gate.md#pending-support-in-structured-walker-input)
  [check?](cargo run -p loom-walk -- template_pinning_matrix_accepts_pending_cells)
- `?` + template-doesn't-include → silent pass (pending);
  `?` + template-includes → walker fails with
  `pending-marker-resolved` so the author drops `?` to `✓` in the
  same diff
  [check?](cargo run -p loom-walk -- pending_addition_marker_fires_when_template_now_includes)
- `~` + template-includes → silent pass (pending);
  `~` + template-doesn't-include → walker fails with
  `pending-marker-resolved` so the author drops `~` to blank in
  the same diff
  [check?](cargo run -p loom-walk -- pending_removal_marker_fires_when_template_no_longer_includes)
- The walker's existing per-cell assertion is unchanged for
  non-pending cells: `✓` requires transitive include; blank
  forbids transitive include; mismatch fails the walker
  [check?](cargo run -p loom-walk -- template_pinning_matrix)

### Planning-rubric pending discipline

- `partial/plan_stage_rubric.md` exists and is included by
  `plan_new.md` and `plan_update.md` only
  [check?](cargo run -p loom-walk -- template_pinning_matrix)
- The partial body distinguishes **binary-pending** from
  **assertion-pending** pending-modifier cases with worked
  examples, so a planning agent author understands both shapes
  warrant `?`
  [check?](grep -qi 'binary-pending\|assertion-pending' crates/loom-templates/templates/partial/plan_stage_rubric.md)
- The partial body names the **"added and modified" rule**
  explicitly — pending discipline applies to annotations the
  session adds AND to annotations whose target the session
  changed in a way that breaks resolution
  [check?](grep -qi 'added.*modified\|added and modified\|modified.*annotation' crates/loom-templates/templates/partial/plan_stage_rubric.md)
- The partial body names the **structured walker input** rule —
  planning edits to matrix / surface / wire-format input use the
  walker's `?` (pending addition) and `~` (pending removal) cell
  syntax for pending elements, not the SC-level `?` modifier, per
  gate.md § Pending support in structured walker input
  [check?](grep -qi 'structured.*input\|pending.*cell\|walker.*input' crates/loom-templates/templates/partial/plan_stage_rubric.md)
- The partial body names the **self-cleaning obligation** — the
  `?` must be dropped in the same diff that resolves the target,
  else `UnneededPendingMarker` fires
  [check](grep -qi 'UnneededPendingMarker\|self-cleaning\|drop the.*marker' crates/loom-templates/templates/partial/plan_stage_rubric.md)

### Review emit shape

- `partial/findings_walk.md` is the single source of truth for the
  `LOOM_FINDING: <json>` streaming wire format and the terminal
  `LOOM_CONCERN: {"summary": "..."}` JSON shape. The partial
  documents the `{"token","bonds","target","evidence"}` finding
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
  (`LOOM_COMPLETE`, `LOOM_NOOP`) and contains no `LOOM_CONCERN:` or
  `LOOM_FINDING:` literal — those belong to `findings_walk.md`
  per the partial split documented in [gate.md § Findings and
  Minting](gate.md#findings-and-minting)
  [check](bash -c "! grep -nE 'LOOM_CONCERN:|LOOM_FINDING:' crates/loom-templates/templates/partial/progress_markers.md")
- `partial/self_report_markers.md` covers the worker-phase self-report
  markers (`LOOM_RETRY`, `LOOM_CLARIFY`, `LOOM_BLOCKED`) and contains
  no `LOOM_CONCERN:` or `LOOM_FINDING:` literal
  [check](bash -c "! grep -nE 'LOOM_CONCERN:|LOOM_FINDING:' crates/loom-templates/templates/partial/self_report_markers.md")
- Interactive-session templates (`plan_new.md`, `plan_update.md`,
  `msg.md`) deliberately **omit** `self_report_markers.md` because
  the worker-phase cannot-finish markers are not valid emit options
  for interactive sessions — the human resolves friction in-turn.
  Including the partial would teach interactive agents about markers
  they cannot emit
  [check?](cargo run -p loom-walk -- template_pinning_matrix)
- The partial body names `LOOM_RETRY` semantics distinctively
  (transient / environmental / agent-self-reset, consumes a
  `[loop] max_retries` slot, escalates to `loom:blocked` cause
  `retry-exhausted` on exhaustion) so a grep for the rule succeeds
  [check?](grep -qi 'LOOM_RETRY' crates/loom-templates/templates/partial/self_report_markers.md)
- The partial body distinguishes `LOOM_BLOCKED` from `LOOM_CLARIFY`:
  blocked = genuine dead end, no candidate resolutions; clarify =
  decision the agent can frame as a structured `## Options — …`
  block. The discriminator (can the agent enumerate options?) is
  named explicitly
  [check](grep -qi 'candidate resolution\|enumerate options' crates/loom-templates/templates/partial/self_report_markers.md)
- The partial body identifies the worker-phase scoping: `LOOM_RETRY`,
  `LOOM_CLARIFY`, `LOOM_BLOCKED` are valid in worker phases (`loop`,
  `todo_*`, `review`) only; interactive sessions (`plan_*`,
  `msg`) emit `LOOM_COMPLETE` only because the human resolves
  friction in-turn
  [check?](grep -qi 'worker.*phase\|interactive.*session' crates/loom-templates/templates/partial/self_report_markers.md)

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
  [check?](grep -q 'AgentRetry' crates/loom-templates/src/previous_failure.rs)
- The `Display for PreviousFailure` rendering of `AgentRetry`
  surfaces the agent's prior `reason` and instructs the retry
  attempt to escalate to `LOOM_BLOCKED` or `LOOM_CLARIFY` if the
  same problem persists after retry
  [test?](agent_retry_display_renders_reason_and_escalation_guidance)
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
  [test?](bad_walk_variants_preserve_max_context_invariant_by_struct_shape)
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
  [test?](bad_walk_concern_display_renders_parsed_findings_digest_when_present)
- The `Display for PreviousFailure` rendering of
  `BadWalk(FindingsWithoutConcern)` appends a per-finding digest of
  `findings` so the agent's next iteration sees the diagnosis it
  just emitted
  [test?](bad_walk_findings_without_concern_display_renders_findings_digest)
- The `Display for PreviousFailure` rendering of
  `BadWalk(MalformedFinding)` enumerates per-line errors AND
  surfaces the well-formed `terminal` via its rendered form so the
  agent fixes the fence/format without losing the surrounding
  context
  [test?](bad_walk_malformed_finding_display_surfaces_terminal_and_per_line_errors)
- `TreeNotClean` variant carries `dirty_paths: Vec<String>` capped
  at 30 entries by the driver before construction
  [check](grep -q 'TreeNotClean' crates/loom-templates/src/previous_failure.rs)
- `PostIntegrateFail` variant carries `failures: Vec<VerifierFailure>`
  directly; populated when the loom-workspace per-bead integration
  step's verify against the integrated tree fails after the bead's
  own verify passed at its bead workspace. Per-bead does not run
  `loom gate review`, so review concerns are not a possible cause —
  they fire at the molecule-completion push gate via
  `GateFailReason` per [harness.md § Verdict Gate](harness.md#verdict-gate)
  [check?](grep -q 'PostIntegrateFail' crates/loom-templates/src/previous_failure.rs)
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
  integration branch, the post-integration audit failed at …")
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

### Public surface

- `templates` exposes `PreviousFailure`, `VerifierFailure`,
  `BadWalk`, `DriverNoticeCause`, `CriterionStatus`,
  `CriterionResult`, `LoopContext`, `ReviewContext`, `PinnedContext`
  as public types consumable from external crates
  [check](cargo run -p loom-walk -- loom_templates_public_types)
- Each partial in the *Partials* table is also exposed as a public
  `&'static str` constant (e.g. `SCRATCHPAD_PARTIAL`,
  `CONTEXT_PINNING_PARTIAL`, etc.) for consumer template composition
  [check](cargo run -p loom-walk -- loom_templates_public_partial_constants)
- Loom's workflow template bodies themselves (`plan.md`, `todo.md`,
  `loop.md`, `review.md`, `msg.md`) are NOT publicly exported —
  only the typed contexts and partial strings
  [check](cargo run -p loom-walk -- loom_templates_workflow_templates_not_exported)

### Criterion-status surface

- `TodoNewContext` and `TodoUpdateContext` carry
  `criterion_status: Vec<CriterionStatus>`; no other phase context
  does
  [check](cargo run -p loom-walk -- todo_contexts_carry_criterion_status)
- `CriterionStatus` is a struct with fields `criterion_anchor`,
  `annotation`, `last_result`, `last_timestamp_ms`, `last_commit`,
  `commits_since`; `CriterionResult` is a tagged enum with
  variants `Pass`, `Fail`, `Skipped`, `NoResult`
  [check](grep -q 'pub struct CriterionStatus' crates/loom-templates/src/criterion_status.rs)
- `todo_new` and `todo_update` rendered prompts surface every
  `CriterionStatus` row's annotation + last result + recency
  signal so the agent can distinguish fresh-pass criteria from
  stale or never-run ones
  [test](todo_templates_render_criterion_status_rows)

### Decomposition discipline

- `partial/decomposition_discipline.md` exists and is included by
  `todo_new.md` and `todo_update.md` only; the body names the
  audit obligation and the two acceptable session outcomes
  [check?](cargo run -p loom-walk -- template_pinning_matrix)
- The partial body names the discipline distinctively (so a grep
  catches accidental emptying)
  [check](grep -qi 'evidence-confirmed\|audit before' crates/loom-templates/templates/partial/decomposition_discipline.md)
- Rendered `todo_new` and `todo_update` prompts contain a clause
  committing the agent to confirm missing work by inspection
  before authoring any non-audit bead
  [test](todo_templates_render_pre_decomposition_audit_clause)
- The partial documents `LOOM_CLARIFY` on the molecule epic as
  the fallback when coverage cannot be determined, with the
  `## Options — …` block per [gate.md](gate.md)'s Options Format
  Contract
  [check](grep -q 'LOOM_CLARIFY' crates/loom-templates/templates/partial/decomposition_discipline.md)
- `todo_new.md` directs the agent to create the molecule epic
  before the gap-analysis pass, so the clarify-on-epic fallback
  has a valid target mid-decomposition
  [check](cargo run -p loom-walk -- todo_new_creates_epic_before_decomposition)

## Requirements

### Functional

1. **Compiled workflow templates.** Every Loom-workflow phase
   prompt (`plan`, `todo`, `loop`, `review`, `msg`) is an
   Askama template compiled into the binary. Template correctness
   is verified at compile time. No per-project mechanism for
   *overriding Loom's workflow templates*; updates ship via a new
   loom release. (Consumers writing their own templates for their
   own LLM calls via `llm` use the public typed building
   blocks per FR12; this FR is specifically about Loom's own
   workflow templates.)
2. **One template per phase plus per-mode variants** as enumerated
   in *Template Files* above.
3. **Partials** as enumerated in *Partials* above. Each partial
   declares which templates include it; the matrix in *Pinning
   Policy* is the authoritative listing.
4. **Typed context per template.** Each template has a Rust
   `#[derive(Template)]` struct with one field per variable. The
   variable set is enumerated in *Template Variables*.
5. **Per-phase pinning.** Partial inclusion follows *Pinning
   Policy*; `style_rules.md` is pinned in `loop` and `review` only;
   `spec_conventions.md` is pinned in `plan_new` and `plan_update`
   only. Matrix cells use the four-value vocabulary `✓` / blank /
   `?` (pending addition) / `~` (pending removal) per
   [gate.md § Pending support in structured walker
   input](gate.md#pending-support-in-structured-walker-input);
   the pinning-matrix walker enforces the assertion at the
   appropriate scope and fails with `pending-marker-resolved`
   when a pending marker's state catches up.
6. **Rule-family agnosticism.** The `style_rules.md` and
   `review_rubric.md` partial bodies discover rule families from
   the pinned `{{ style_rules }}` document. Template bodies do
   not enumerate fixed prefixes.
7. **Agent-output markers.** All agent-generated content rendered
   back into a prompt is wrapped in `<agent-output>` /
   `</agent-output>`.
8. **Snapshot tests.** Every template × representative-input
   combination has an `insta` snapshot.
9. **Typed `PreviousFailure`** — `LoopContext.previous_failure` is
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
10. **Attempt counter.** `LoopContext.attempt: u32` is the per-bead
    in-session retry counter, bounded by `[loop] max_retries`
    (default 2), resets to 0 on fresh bead dispatch. Fix-up beads
    start at `attempt = 0`; molecule-level iteration is opaque to
    the agent. `loop.md` renders the attempt line when `attempt > 0
    && previous_failure.is_some()`, omits it otherwise.
11. **First-instruction reframe.** When
    `previous_failure.is_some()`, `loop.md` prepends "Re-read the
    previous failure block above and address its specific concern
    before re-implementing." Single generic form — per-variant
    detail lives in the previous-failure block itself.
12. **Public surface for consumers.** `templates` is a
    public-contract crate. Exposed: `PreviousFailure` (and its
    sub-types), `LoopContext`, `ReviewContext`, `PinnedContext`,
    and the partial-string constants for each entry in the
    *Partials* table. Loom's workflow template bodies themselves
    are not exposed — consumers compose their own templates from
    the typed contexts + partial strings, not from Loom's workflow
    templates. Stability: additive type changes are minor bumps;
    removing or renaming fields / partial paths is a major bump.

    **Dependency on `loom-protocol`.** The typed gate wire-format
    contract (`Finding`, `ConcernToken`, `FindingTarget`, `BadWalk`,
    `WalkOutput`, etc.) lives in `loom-protocol::gate` — see
    [gate.md § Canonical contract location](gate.md#canonical-contract-location).
    `loom-templates` depends on `loom-protocol` so
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
    workflow template bodies themselves are not exposed" rule at
    the head of this FR12), not a divergent loading mechanism.

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
13. **Chat discipline in interactive sessions.**
    `partial/chat_interview.md`, pinned in every interactive-session
    template (`plan_new`, `plan_update`, `msg`), requires the
    interactive agent to conduct conversations as back-and-forth
    prose and forbids Claude Code's structured option-picker tool
    (`AskUserQuestion` or any equivalent multi-choice widget).
    Options are listed inline in prose; the user replies in prose.
    The partial also carries the **persistence-destination clause**:
    session-bridging memory (decisions, context, follow-ups) goes
    into bd (notes, descriptions, new beads) or spec files, not
    Claude Code's `MEMORY.md` system which is container-local and
    disappears with the container. The "one by one" sub-mode is
    planning-specific and lives in a separate partial; the chat-
    discipline rules above apply to every interactive session,
    including msg-chat.
14. **Criterion-status surface for decomposition.** `todo_new` and
    `todo_update` contexts carry `criterion_status:
    Vec<CriterionStatus>` where each `CriterionStatus` exposes
    annotation target + last result (`Pass | Fail | Skipped |
    NoResult`) + last timestamp + last commit + commits-since-HEAD.
    The driver populates the surface from
    [gate.md](gate.md#status-cache)'s sqlite cache. No new cache
    schema; the existing fields suffice. The struct does not
    encode staleness thresholds — the partial body owns the
    heuristic.
15. **Decomposition discipline in `todo_*` phases.**
    `partial/decomposition_discipline.md`, pinned in `todo_new`
    and `todo_update` only, requires the decomposition agent to
    confirm missing work by consulting `criterion_status` and (for
    suspicious or empty cache rows) reading representative
    implementations before authoring any non-audit bead. The
    partial defines the two acceptable session outcomes:
    (a) a gap-targeted bead set citing evidence per bead, or
    (b) `LOOM_CLARIFY` on the **molecule epic** with the
    `## Options — …` block when coverage cannot be determined.
    `todo_new.md` creates the molecule epic before the
    gap-analysis pass — without an existing epic the
    clarify-on-epic fallback has no target.
16. **Self-report marker taxonomy.** The worker-phase self-report
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
      Routes to `loom:clarify` for human resolution via `loom msg`.
    - `LOOM_BLOCKED` — genuine dead end: the agent cannot proceed
      and has no candidate resolutions to enumerate. Routes to
      `loom:blocked`; `loom msg -c` walks the human through
      candidate enumeration in-session.

    The semantic discriminator between the three is explicit and
    grep-able in the partial body: "expect retry to succeed? →
    RETRY. can you enumerate options? → CLARIFY. dead end? → BLOCKED."
    The taxonomy applies to worker phases only (`loop`, `todo_*`,
    `review`); interactive sessions (`plan_*`, `msg`) emit
    `LOOM_COMPLETE` only — the human resolves friction in-turn.
17. **Options-block requirement on clarify-bound findings.**
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
- **Override of Loom's workflow templates.** Loom's `plan` /
  `todo` / `loop` / `review` / `msg` templates are Askama,
  compiled into the binary. There is no per-project template-fetch
  or template-tune mechanism for overriding *Loom's own* templates;
  template updates ship via a new loom release. Project-specific
  prompt tweaks to Loom's workflow happen via `pinned_context` /
  `style_rules` / `spec_conventions` configuration and per-spec
  implementation notes. Consumers writing their *own* templates
  (for their own LLM calls via `llm`) compose them from the
  exposed typed building blocks (above) — that path is supported
  and is *not* what this exclusion covers.
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

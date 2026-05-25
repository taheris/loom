# Loom Templates

Askama template engine, partials inventory, per-phase pinning
policy, snapshot-test contract, and public-contract typed building
blocks consumers compose into their own templates.

## Problem Statement

Loom's agent-bearing workflow phase prompts (`plan`, `todo`, `run`,
`review`, `msg`) are rendered from Askama templates compiled into
the binary. `loom gate verify` is deterministic and renders no
template. The template surface is its own concern: which partials
exist, which template renders which partial in which phase, which
context struct each template binds to, and what the snapshot gate
looks like.

`templates` is a **public-contract crate**: external Rust
consumers depending on `llm` for typed LLM calls can compose
their own templates using `templates`' exposed typed context
structs (`PinnedContext`, `PreviousFailure`, `RunContext`, etc.) and
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
- `run.md`, `review.md`, `msg.md`

`loom gate verify` is deterministic — it runs verifiers, audits,
and linters without rendering any agent prompt — so it has no
template. `loom gate review` is the LLM-judged counterpart and
has its own template, distinct from `run.md` because the review
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
| `exit_signals.md` | Document the `LOOM_*` exit markers the phase accepts. **Markers are mutually exclusive — exactly one per session, on the final line.** For review-phase sessions: emit `LOOM_CONCERN: <token> -- <reason>` when a concern is found (review-phase only marker per [harness.md](harness.md#verdict-gate)), or `LOOM_COMPLETE` when the review is clean — **never both**. The earlier `LOOM_REVIEW_FLAG` name is retired; consumers and templates use `LOOM_CONCERN`. |
| `chat_marker_final_turn_only.md` | Restrict `LOOM_COMPLETE` emission to the **final** assistant turn of a multi-turn chat. Included by `msg`, `plan_new`, and `plan_update` to disambiguate `exit_signals.md`'s "end your response with the marker" language (which is correct for single-shot worker phases but misreads as "every response" in chat). One-shot worker phases (`run`, `todo_*`, `review`) deliberately omit it because every response in those phases IS the final output. |
| `interview_modes.md` | Describe the "one by one" / "polish the spec" interview sub-modes |
| `plan_stage_rubric.md` | Gate the planning interview on completeness / coherence / invariant-clash before any commit |
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

Each partial is included by an explicit set of templates:

| Partial | `plan_new` | `plan_update` | `todo_new` | `todo_update` | `run` | `review` | `msg` |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| `context_pinning.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `style_rules.md` |  |  |  |  | ✓ | ✓ |  |
| `spec_conventions.md` | ✓ | ✓ |  |  |  |  |  |
| `spec_header.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |  |
| `companions_context.md` |  | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `scratchpad.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `exit_signals.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `chat_marker_final_turn_only.md` | ✓ | ✓ |  |  |  |  | ✓ |
| `interview_modes.md` | ✓ | ✓ |  |  |  |  |  |
| `plan_stage_rubric.md` | ✓ | ✓ |  |  |  |  |  |
| `invariant_clash.md` | ✓ | ✓ |  |  |  |  |  |
| `review_rubric.md` |  |  |  |  |  | ✓ |  |
| `sibling_spec_editing.md` |  | ✓ |  |  |  |  |  |

**`style_rules.md` is pinned only in `run` and `review`** — the two
phases that write or evaluate code (`run` produces it, `review`
judges it). Other phases (planning, decomposition, clarify
resolution) don't write or evaluate code, so pinning the rules
there would inflate prompt size without buying enforcement.

**`spec_conventions.md` is pinned only in `plan_new` and
`plan_update`** — the two phases that author spec content. Other
phases consume specs but don't modify them.

### Template Variables

Each variable is bound to a typed field on the relevant context
struct. `String`-typed values arriving from beads or config flow
through the parse-don't-validate boundary defined in
[harness.md](harness.md#parse-dont-validate).

| Variable | Type | Used By |
|----------|------|---------|
| `pinned_context` | `String` | all |
| `style_rules` | `String` | `run`, `review` |
| `spec_conventions` | `String` | `plan_new`, `plan_update` |
| `label` | `SpecLabel` | all |
| `spec_diff` | `Option<String>` | `todo_update` |
| `existing_tasks` | `Option<String>` | `todo_update` |
| `companion_paths` | `Vec<String>` | `plan_update`, `todo_*`, `run`, `review`, `msg` |
| `clarify_beads` | `Vec<ClarifyBead>` | `msg` |
| `implementation_notes` | `Vec<String>` | `todo_new`, `todo_update` |
| `molecule_id` | `Option<MoleculeId>` | `todo_update`, `run` |
| `issue_id` | `Option<BeadId>` | `run` |
| `title` | `Option<String>` | `run` |
| `description` | `Option<String>` | `run` |
| `previous_failure` | `Option<PreviousFailure>` | `run` (retry only; typed enum — see *Typed `PreviousFailure`* below) |
| `review_notes` | `Option<String>` | `run` (set only when `previous_failure` is `VerifyFailures` and review also raised a concern) |
| `attempt` | `u32` | `run` (in-session per-bead retry counter — see *Attempt Counter* below) |
| `beads_summary` | `Option<String>` | `review` |
| `base_commit` | `Option<String>` | `review` |
| `exit_signals` | `String` | all |
| `scratchpad_path` | `String` | all |

The newtypes (`SpecLabel`, `MoleculeId`, `BeadId`) are
architecture-bearing types defined in
[harness.md](harness.md#parse-dont-validate); the
template treats them as opaque typed values.

`implementation_notes` is sourced from the state DB's `notes` table
(kind = `implementation`); see *Notes lifecycle* in
[harness.md](harness.md#sqlite-state-store).

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

    /// Review LLM flagged a semantic concern.
    ReviewConcern { concern: ReviewConcernKind, reason: String },

    /// Pre-verifier build/compile failure (agent's code didn't compile).
    BuildFailure { stage: String, output: String },
}

pub enum DriverNoticeCause {
    SwallowedMarker,
    IncompleteSignaling,
    ZeroProgress,
    ObserverAbort,
    RetryExhausted,
}

pub struct VerifierFailure {
    pub target: String,       // e.g. "cargo test ... -- my_test"
    pub exit_code: i32,
    pub stderr_tail: String,  // ~last 40 lines, capped per-block
}

pub enum ReviewConcernKind {
    SpecCoherence,
    OrphanIntegration,
    VerifierBypass,
    FabricatedResult,
    WeakAssertion,
    CoincidentalPass,
    MockDiscipline,
    VerifierTooNarrow,
    ConcurrencyUntested,
    ScopeCreep,
    ScopeShortfall,
    JudgeFlag,
    Other(String),  // forward-compatible fallback
}
```

The full set of `ReviewConcernKind` variants is defined in
[gate.md](gate.md); the `Other` arm keeps the type
forward-compatible when gate.md grows new flag causes.

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
- `ReviewConcern` → `"Review raised a concern ({concern}): {reason}"`
- `BuildFailure` → `"Build failed at {stage}:\n{output}"`
- `review_notes` (when set, after the primary block) → heading `"Review notes:"` then content

Driver maps verdict-gate causes to variants per the table in
[harness.md — Verdict Gate](harness.md#verdict-gate).

### Attempt Counter

`attempt` is the per-bead in-session retry counter, populated by
the driver and rendered by `run.md`:

- `attempt == 0` on fresh bead dispatch — no retry context, no
  attempt line in the template
- Each in-session retry increments `attempt` (bounded by
  `[loop] max_retries`, default 2)
- Resets to 0 when a new bead is dispatched (fix-up beads carry
  fresh prompts, not retry state from the failing bead)
- **Molecule-level iteration is opaque to the agent** — fix-up
  beads are different prompt contexts, and a counter that spans
  them would be misleading

When `attempt > 0 && previous_failure.is_some()`, `run.md`
prepends a counter line: `"Retry attempt {attempt} — previous
attempt failed with: …"` followed by the typed
`previous_failure` block.

### First-instruction reframe

When `previous_failure.is_some()`, `run.md` prepends to its first
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
- `PreviousFailure`, `VerifierFailure`, `ReviewConcernKind`,
  `DriverNoticeCause` (the typed retry-context surface)
- `RunContext`, `ReviewContext` (workflow-phase context shapes
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
- Loom's workflow templates (`plan.md`, `todo.md`, `run.md`,
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
`<workspace>/config.toml`:

```toml
# Project overview — pinned in every phase
pinned_context = "docs/README.md"

# Style rules — pinned in run and check
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
  [check](grep -q 'partial/context_pinning' crates/loom-templates/templates/run.md)
- Rendered output is stable across runs for identical inputs,
  verified by `insta` snapshots
  [test](template_renders_are_byte_stable_across_runs)

### Pinning policy

- `style_rules.md` partial renders the `style_rules` variable
  [check](grep -q '{{ style_rules' crates/loom-templates/templates/partial/style_rules.md)
- `run.md` and `review.md` include `style_rules.md`; no other
  phase template does
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- `spec_conventions.md` partial renders the `spec_conventions`
  variable; included only by `plan_new` and `plan_update`
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- `RunContext` and `ReviewContext` carry `style_rules: String`;
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
- Every cell of the pinning matrix above matches the actual
  `{% include %}` graph in `loom-templates/templates/` (transitive
  resolution); drift in either direction — `✓` with no include or
  include with no `✓` — fails the audit
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- The `chat_marker_final_turn_only.md` partial is included by
  every multi-turn template (`msg`, `plan_new`, `plan_update`),
  pinning the "emit `LOOM_COMPLETE` on the final turn only" rule
  that disambiguates `exit_signals.md`'s single-shot wording
  [test](every_multi_turn_template_includes_chat_marker_partial)
- One-shot worker templates (`run`, `todo_new`, `todo_update`,
  `review`) deliberately **omit** `chat_marker_final_turn_only.md`
  because every response in those phases is the session's final
  output; including the chat-only clause would mislead the agent
  into withholding the marker
  [test](worker_templates_omit_chat_final_turn_clause)

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

### Typed `PreviousFailure`

- `PreviousFailure` is a tagged enum with variants `DriverNotice`,
  `VerifyFailures(Vec<VerifierFailure>)`, `ReviewConcern`, and
  `BuildFailure` — not a free string
  [check](grep -q 'pub enum PreviousFailure' crates/loom-templates/src/previous_failure.rs)
- `DriverNoticeCause` enum covers `SwallowedMarker`,
  `IncompleteSignaling`, `ZeroProgress`, `ObserverAbort`,
  `RetryExhausted`
  [check](grep -q 'pub enum DriverNoticeCause' crates/loom-templates/src/previous_failure.rs)
- `ReviewConcernKind` enum carries all 12 named variants from
  gate.md plus `Other(String)` fallback
  [check](grep -q 'pub enum ReviewConcernKind' crates/loom-templates/src/previous_failure.rs)
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
  `ReviewConcern` → "Review raised a concern (...):", `BuildFailure` →
  "Build failed at ...:")
  [test](previous_failure_variant_framings_match_spec)

### Attempt counter

- `RunContext` carries `attempt: u32`; field is `0` on fresh
  bead dispatch
  [test](attempt_zero_on_fresh_bead_dispatch)
- `run.md` omits the attempt line when `attempt == 0`
  [test](run_template_omits_attempt_line_when_zero)
- `run.md` renders "Retry attempt {N} — previous attempt failed
  with: …" when `attempt > 0 && previous_failure.is_some()`
  [test](run_template_renders_attempt_line_on_retry)
- Attempt counter is per-bead in-session: fix-up beads start at
  `attempt = 0` regardless of the failing bead's prior attempts
  [test](fix_up_bead_starts_at_attempt_zero)
- Attempt counter is bounded by `[loop] max_retries` (default 2)
  [test](failed_bead_retries_with_previous_failure_then_clarifies)

### First-instruction reframe

- `run.md` prepends "Re-read the previous failure block above and
  address its specific concern before re-implementing." when
  `previous_failure.is_some()`
  [test](run_template_prepends_first_instruction_reframe_on_retry)
- Reframe is omitted when `previous_failure.is_none()`
  [test](run_template_omits_first_instruction_reframe_on_fresh_dispatch)
- Reframe wording is generic (one form regardless of variant);
  per-variant detail lives inside the previous-failure block itself
  [check](grep -q 'Re-read the previous failure block above' crates/loom-templates/templates/run.md)

### Public surface

- `templates` exposes `PreviousFailure`, `VerifierFailure`,
  `ReviewConcernKind`, `DriverNoticeCause`, `RunContext`,
  `ReviewContext`, `PinnedContext` as public types consumable from
  external crates
  [check](cargo run -p loom-walk -- loom_templates_public_types)
- Each partial in the *Partials* table is also exposed as a public
  `&'static str` constant (e.g. `SCRATCHPAD_PARTIAL`,
  `CONTEXT_PINNING_PARTIAL`, etc.) for consumer template composition
  [check](cargo run -p loom-walk -- loom_templates_public_partial_constants)
- Loom's workflow template bodies themselves (`plan.md`, `todo.md`,
  `run.md`, `review.md`, `msg.md`) are NOT publicly exported —
  only the typed contexts and partial strings
  [check](cargo run -p loom-walk -- loom_templates_workflow_templates_not_exported)

## Requirements

### Functional

1. **Compiled workflow templates.** Every Loom-workflow phase
   prompt (`plan`, `todo`, `run`, `gate review`, `msg`) is an
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
   Policy*; `style_rules.md` is pinned in `run` and `review` only;
   `spec_conventions.md` is pinned in `plan_new` and `plan_update`
   only.
6. **Rule-family agnosticism.** The `style_rules.md` and
   `review_rubric.md` partial bodies discover rule families from
   the pinned `{{ style_rules }}` document. Template bodies do
   not enumerate fixed prefixes.
7. **Agent-output markers.** All agent-generated content rendered
   back into a prompt is wrapped in `<agent-output>` /
   `</agent-output>`.
8. **Snapshot tests.** Every template × representative-input
   combination has an `insta` snapshot.
9. **Typed `PreviousFailure`** — `RunContext.previous_failure` is
   `Option<PreviousFailure>` where `PreviousFailure` is a tagged
   enum (`DriverNotice`, `VerifyFailures`, `ReviewConcern`,
   `BuildFailure`). The driver populates the right variant from
   the verdict-gate cause classification. Each variant renders
   with distinct framing per *Typed `PreviousFailure`* above.
   Caps: `PREVIOUS_FAILURE_MAX_LEN = 4000` total; per-block stderr
   tail ~1500 chars; `review_notes` separate ~1000-char budget.
10. **Attempt counter.** `RunContext.attempt: u32` is the per-bead
    in-session retry counter, bounded by `[loop] max_retries`
    (default 2), resets to 0 on fresh bead dispatch. Fix-up beads
    start at `attempt = 0`; molecule-level iteration is opaque to
    the agent. `run.md` renders the attempt line when `attempt > 0
    && previous_failure.is_some()`, omits it otherwise.
11. **First-instruction reframe.** When
    `previous_failure.is_some()`, `run.md` prepends "Re-read the
    previous failure block above and address its specific concern
    before re-implementing." Single generic form — per-variant
    detail lives in the previous-failure block itself.
12. **Public surface for consumers.** `templates` is a
    public-contract crate. Exposed: `PreviousFailure` (and its
    sub-types), `RunContext`, `ReviewContext`, `PinnedContext`,
    and the partial-string constants for each entry in the
    *Partials* table. Loom's workflow template bodies themselves
    are not exposed — consumers compose their own templates from
    the typed contexts + partial strings, not from Loom's workflow
    templates. Stability: additive type changes are minor bumps;
    removing or renaming fields / partial paths is a major bump.

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
  `todo` / `run` / `gate review` / `msg` templates are Askama,
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
- **Untyped `previous_failure`.** `RunContext.previous_failure` is
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

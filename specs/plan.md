# Specification interviews

Conducts planning-only interviews, resolves intent and invariant clashes, and
edits checkable contracts with explicit commit consent.

## Problem Statement

Conducts planning-only interviews, resolves intent and invariant clashes, and
edits checkable contracts with explicit commit consent. This package is one
contract owner, not a new crate or command tree.

## Architecture

Inputs, outputs, and trust boundaries are stated in the contracts below. Related
owners: [specs](specs.md), [templates](templates.md), [gate](gate.md),
[todo](todo.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Interview Modes

[Acceptance](#interview-authority-and-modes).

`partial/interview_modes.md` is included by `plan.md` only. It defines
planning-session shorthand that changes how the interview proceeds, not
permission to modify files.

- **Polish** phrases (`"polish the spec"`, `"do a polish pass"`,
  `"do a polish"`) mean a report-only spec/doc review: read the relevant spec
  end-to-end, identify readability, consistency, ambiguity, and structure
  findings, and propose edits. The agent does not apply edits during a polish
  pass unless the user explicitly asks for application.
- **One by one** phrases mean the agent presents one open design question at a
  time, proposes a default with rationale, and waits for the user's prose answer
  before moving to the next question.

These definitions are active instruction context. Compaction recovery is
responsible for preserving them through the pinned prompt; see
[Templates — Compaction Recovery](templates.md#compaction-recovery).

### Planning-Rubric Pending Discipline

[Acceptance](#planning-coverage-and-strategy).

`partial/plan_stage_rubric.md` is pinned in `plan.md`. It owns the planning
interview's pre-commit gate (completeness / coherence / invariant-clash) **and**
the pending-modifier discipline that determines whether the planning session's
spec edits can pass the push gate. Its completeness guidance applies
[Gate's package-scoped coverage rule](#plan-stage-checks): contract sections and
behavioral table rows map to acceptance in the owning package's `tests.md`,
using ordinary Markdown section links under
[spec conventions](../docs/spec-conventions.md#contract-coverage), without
requiring duplicate annotations in `spec.md` or a mapping registry.

The partial body MUST spell out the pending-modifier discipline unambiguously,
because the planning session's biggest failure mode is _spec edits that point at
not-yet-existing verifier targets, which then fail the pre-push
`loom gate verify` and block landing the plan_. The discipline lives in
[Verify — Pending modifier](verify.md#pending-modifier) and its sub-rule
[Verify](verify.md#pending-support-in-structured-walker-input) — the partial
body distills both for the planning agent with the following clauses, each
grep-able by an integrity verifier so the partial cannot quietly drift:

1. **Both binary-pending AND assertion-pending are pending.** The partial
   enumerates both shapes explicitly:
   - **Binary-pending** — the verifier executable or path doesn't exist yet
     (e.g. `[check?](cargo run -p my-future-walker ...)`, or
     `[check?](grep -q ... crates/foo/src/file_that_will_exist.rs)`).
   - **Assertion-pending** — the verifier executable exists but the asserted
     condition doesn't hold yet (e.g.
     `[check?](grep -q 'pub enum NewVariant' crates/foo/src/existing_file.rs)`
     where the file exists but the new symbol hasn't been added).

   Both shapes use the same `?` modifier. For command tiers, a spawn failure or
   non-zero exit silent-passes under `loom gate verify`; exit 0 fires
   `UnneededPendingMarker` because the pending condition has resolved.

2. **"Added" and "modified" annotations both count.** The partial names this
   explicitly, with a worked example: _"if you changed an annotation's command —
   a file path, a grep pattern, a symbol name — and the new target doesn't
   resolve in the current tree, mark it `[tier?]` even though the annotation
   itself isn't new. The integrity gate doesn't distinguish 'new claim' from
   'modified claim'; it checks whether the target resolves now."_ This prevents
   the failure mode where a planning agent only `?`-marks net-new SCs and
   forgets that path swaps on existing SCs need it too.

3. **Structured walker input uses `?` and `~` cells.** When the planning session
   edits structured input read by a sweeping walker (the pinning-matrix cell
   values, the FR1 command-set entries the surface-conformance walker reads, the
   canonical- partial path the anti-drift wire-format walker reads), the pending
   value is `?` (pending addition — will resolve to the present marker) or `~`
   (pending removal — will resolve to the absent marker) _in the input element
   itself_, not in the SC annotation. Per
   [Verify — Pending support in structured walker input](verify.md#pending-support-in-structured-walker-input),
   the walker silent-passes pending elements whose state matches the pending
   direction and fires `pending-marker-resolved` when the state catches up. This
   is the structural answer to the sweeping-walker case; the partial body cites
   the Verify rule and walks the agent through identifying which spec edits
   affect structured walker input.

4. **Self-cleaning is mandatory.** When the implementation lands, the pending
   marker (`?` for `[tier?]` annotations, `?` or `~` for structured walker
   cells) must be dropped in the same diff that resolves the target —
   `UnneededPendingMarker` for annotations, `pending-marker-resolved` for
   structured walker cells. The planning prompt names this so the agent doesn't
   author pending markers as fire-and-forget.

The partial body's text follows the standard one-line-per-rule shape pinned by
the other discipline partials (`chat_interview.md`,
`decomposition_discipline.md`); each numbered clause above maps to a labelled
paragraph in the partial body that the `loom gate check` walker greps for.

### Sibling-Spec Editing

[Acceptance](#interview-authority-and-modes).

`partial/sibling_spec_editing.md` is included in `plan.md`. It tells the
planning agent:

1. Any labels passed to `loom plan [SPEC_LABEL ...]` are **anchors**: they seed
   initial context only and do not define the touched set.
2. During this session, the agent may read and edit any spec in `specs/` when a
   change cross-cuts sibling specs. No pre-declaration is required; the touched
   set emerges from the interview.
3. **Creating a new sibling spec is a valid outcome** when the planner judges
   that a section warrants its own spec. The planner creates the owning spec in
   the
   [supported authoring form](../docs/spec-conventions.md#bootstrap-authoring-before-cutover)
   and records its index entry in `docs/README.md`; it does **not** allocate a
   bead/epic. `loom todo` creates the spec epic and work epic later during
   deterministic preflight.
4. **Commits are never automatic.** Planning sessions edit specs in place but do
   not commit. Soft signals ("looks good", "accept") authorize the next
   interview step, not a commit. Commits happen only on unambiguous trigger
   ("commit", "land the plane", "push it"). The same discipline applies to
   `git push`, `wrix beads push`, and any operation that mutates shared state.

## Success Criteria

### Planning, coverage, and strategy

- Planning and semantic review assess contract coverage across `spec.md` and its
  package's `tests.md`, flagging unmapped behavioral claims and table rows or
  linked criteria that do not cover them. Annotations need not be duplicated in
  contract prose, and unrelated bindings or matching counts do not establish
  coverage. [judge?](../tests/judges/package-contract-coverage.md)

### Interview authority and modes

- Planning guidance and observed interview actions preserve planning-only
  authority and explicit commit consent: soft acknowledgements do not authorize
  commits or publication, polish is report-only unless edits are requested, and
  one-by-one mode asks one prose question with a suggested default before
  waiting. [judge?](../tests/judges/planning-interview-authority.md)

### Workflow commands

- `loom plan [SPEC_LABEL ...]` spawns an interactive container with the base
  profile and runs the spec interview. Positional labels are optional initial
  anchors (existing specs are pinned; missing labels are proposed new specs).
  Options may appear before, between, or after labels. Plan edits spec/index
  markdown and implementation notes only — no bd writes and no touched-set
  manifest [test](plan_accepts_optional_anchor_labels_and_interspersed_options)

### Verdict gate

- `LOOM_RETRY` from an interactive session (`plan`, `inbox`) is a
  wrong-phase-marker error; the driver exits non-zero with a diagnostic and does
  not apply any label
  [test](retry_marker_from_interactive_phase_is_wrong_phase_marker)

### Cache database

- `loom plan [labels...]` does NOT create epics and does NOT write to bd; plan
  sessions edit specs/index/notes only
  [test](plan_does_not_create_epic_or_touch_bd)

- `loom plan [labels...]` reads existing implementation notes for anchor/touched
  specs and writes back merged arrays via `loom note set` (interview-driven
  keep/drop/add — not blind append, not blind replace)
  [judge](../tests/judges/loom.sh#judge_plan_merges_notes)

## Requirements

### Command entrypoint

[Acceptance](#workflow-commands).

- `loom plan [SPEC_LABEL ...]` — spec interview (interactive agent session).
  Positional labels are optional initial anchors, not the touched set: zero
  labels starts from the overview/index; existing labels pin those spec bodies;
  missing labels are proposed new specs. Options may appear before, between, or
  after labels. Plan sessions edit specs/index/notes only — they do **not**
  create epics or write to bd.

##### Plan-stage checks

[Acceptance](#planning-coverage-and-strategy).

The plan stage is first-class: errors caught before code exists are cheapest.
The stage runs inside the planning interview — the agent's rubric. Three checks
must satisfy before the interview can commit:

1. **Completeness check.** Every requirement the user expressed has a checkable
   surface: a Success Criteria bullet in the package's `tests.md` with exactly
   one `[check]`, `[test]`, `[system]`, or `[judge]` annotation, or an explicit
   `## Out of Scope` declaration in `spec.md`. Contract sections and behavioral
   table rows map to those criteria under
   [Spec conventions — Contract coverage](../docs/spec-conventions.md#contract-coverage),
   without duplicating annotations beside contract prose. Implicit functional
   assumptions become checkable claims; genuine non-goals move to Out of Scope
   with their rationale rather than remaining as unverified positive contracts.
   Annotations whose targets will not resolve at commit time — typically
   newly-authored claims whose verifier implementation lands in a follow-on
   `loom loop` bead — carry the pending modifier `?` (see
   [_Pending modifier_](verify.md#pending-modifier)). Applying the marker is
   part of completeness: an unmarked annotation pointing at a not-yet-existing
   target reads as a broken claim, where a `?`-marked annotation reads as an
   honest declaration of the surface plus an explicit acknowledgement that the
   implementation is on the way.
2. **Internal coherence check.** The spec under interview is scanned for
   internal contradiction — two sections saying different things, decision-table
   rows that conflict, prose claims that can't both be true.
3. **Invariant-clash scan.** Check the anchor and any touched sibling specs for
   invariants the proposed change may contradict (architectural / data-structure
   / explicit-constraint / non-functional / out-of-scope). On detection, pause;
   resolve via three paths.

The agent doesn't separately _run_ the gate at this stage — the gate IS the
agent's rubric. A check failing means the interview stays open until the user
resolves it.

(General agent discipline: at any stage, if the agent notices the template it's
running under contradicts the spec, it raises the contradiction as a user
question. This isn't a structured rubric item at the plan stage — it's expected
awareness. Mechanical detection of template-vs-spec drift happens at the
standing safety net instead.)

#### loom-workflow

[Acceptance](#success-criteria).

- `loom plan [SPEC_LABEL ...]` anchor parsing, including zero anchors, multiple
  anchors, missing-label-as-new-spec, and interspersed options

## Out of Scope

- Code/model implementation, Beads mutation, automatic commits, and automatic
  publication are outside planning authority. An acknowledgement does not
  authorize a commit.

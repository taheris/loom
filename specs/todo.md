# Spec decomposition

Derives changed-spec work from durable cursors, decomposes it, and finalizes
typed task assignments and work epics.

## Problem Statement

Derives changed-spec work from durable cursors, decomposes it, and finalizes
typed task assignments and work epics. This package is one contract owner, not a
new crate or command tree.

## Architecture

Inputs, outputs, and trust boundaries are stated in the contracts below. Related
owners: [specs](specs.md), [plan](plan.md), [loop](loop.md),
[harness](harness.md), [templates](templates.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

#### Binding proposal and persistence

[Acceptance](#success-criteria).

The todo agent proposes which criteria each task addresses, using criterion
identities supplied in its context. Those assignments accompany the decomposed
tasks in the typed todo handoff; they are not inferred from task descriptions.
The Rust driver parses the proposals, resolves their criterion references
against the todo preflight snapshot, and checks that their target tasks belong
to the accepted decomposition before writing bindings. This resolution checks
referential integrity, not the semantic adequacy of the agent's task
decomposition.

The Rust driver owns serialization and persistence of these bindings into
structured Beads task metadata during todo finalization. It serializes a typed
reference payload through Serde and the existing Beads boundary, preserving
unrelated task metadata; the agent does not issue hand-crafted metadata writes
for these bindings. Task descriptions remain prose. Metadata stores criterion
references, not copied requirement text, verifier commands, cached verdicts, or
a claim that resolution has already succeeded for a future snapshot.

Serde ingestion produces typed references with checked identifiers, never an
already-resolved acceptance value supplied by external JSON. Snapshot resolution
is a separate fallible construction step. Invalid assignments or failure to
persist required bindings prevent successful todo finalization: the new batch
cannot become active, cursors cannot advance, and implementation notes cannot be
consumed on that basis. Existing finalization guarantees still apply.

## Spec and Work Epic Lifecycle

[Acceptance](#success-criteria).

A spec epic is the durable metadata carrier for one indexed spec, labelled
`loom:spec` and `spec:<label>`. Exactly one exists per indexed spec. Its
`loom.todo_cursor` records the Git commit through which decomposition was
finalized; status does not affect lookup, and implementation beads are not
parented beneath it.

A work epic is an execution batch. `loom todo` creates or reuses one pending
`loom:todo` epic for the deterministic changed-spec roster. A validated
`LOOM_TODO` handoff applies its final title, advances every roster spec's cursor
atomically, removes `loom:todo`, and makes it the sole `loom:active` epic.
Standing tree remediation similarly creates a non-empty active work epic only
after actionable findings exist. `loom:active` selects the default loop root; it
does not select changed specs for todo.

Todo preflight derives changed specs from the current index, package contents
and tracked supporting inputs, Git ancestry, and each spec epic's cursor. It
ensures one spec epic per indexed spec, surfaces missing or invalid durable
metadata, parses every changed spec's criteria, and represents absent cache
evidence as missing. No changed specs means no agent, no work epic, and no
cursor movement.

Todo success is the public `loom-protocol::todo::TodoSuccess` wire value. It
binds the preflight head and fingerprint, pending work epic, final non-empty
title, and exactly one outcome for each changed spec. A decomposed outcome names
non-empty child beads under the work epic and carries their proposed acceptance
assignments under
[Task acceptance references](specs.md#task-acceptance-references); a no-work
outcome gives a non-empty reason. Successful finalization includes driver
persistence of admitted assignments. Validation or binding-persistence failure
leaves pending state and every cursor unchanged.

Implementation notes are typed, transient cache hints. Planning may merge them;
validated todo finalization renders and consumes notes for the changed specs.
Failed or non-finalized decomposition leaves them intact. Rebuild drops them
without changing durable workflow truth.

### Todo Success Marker

[Acceptance](#success-criteria).

`partial/todo_success.md` instructs the agent that a successful todo session
ends with exactly one final line:

```text
LOOM_TODO: {"head":"<sha>","fingerprint":"<fingerprint>","work_epic":"<bead-id>","title":"<final work epic title>","specs":[...]}
```

The JSON shape is derived from `loom-protocol::todo::TodoSuccess` as specified
in [Todo — Spec and Work Epic Lifecycle](#spec-and-work-epic-lifecycle). The
template tells the agent to include a required non-empty final work- epic title
plus exactly the changed specs the driver injected, using `Decomposed { beads }`
for non-empty work and `NoWork { reason }` for an audited no-implementation
outcome. `Blocked`, `pending`, or omitted specs are not success states; the
agent emits `LOOM_CLARIFY` or `LOOM_BLOCKED` instead.

For decomposed tasks, the prompt asks the agent to propose task-to-criterion
assignments using the injected criterion identities and the shared typed todo
handoff. It does not ask the agent to compute IDs or write binding metadata with
`bd`. [Todo](#binding-proposal-and-persistence) owns the Rust resolution,
serialization, and persistence boundary; the template does not maintain a
separate Beads JSON schema. Ordinary task creation, descriptions, labels, and
dependencies remain part of decomposition.

### Decomposition Discipline

[Acceptance](#decomposition-discipline-1).

`partial/decomposition_discipline.md` is included by `todo.md`. It tells the
decomposition agent that the driver has already computed the exact changed-spec
roster and created or reused the `loom:todo` work epic. The agent must decompose
**that roster exactly**; it does not discover or narrow the changed-spec set.

Before authoring any non-audit bead, the agent must:

1. Consult the `criterion_status` surface (see _Criterion-Status Surface_) for
   each criterion in each changed spec.
   `EvidenceState::Current { result: Pass, commits_since: 0, ... }` is positive
   evidence of coverage; `Missing` or `StaleAnnotation` is absence/staleness of
   evidence, not a reason to treat the criterion as already complete.

2. Read representative existing implementations and verifier functions for
   criteria where evidence is missing, stale, failed, skipped, or the agent
   judges the verifier target may not exercise the live system per
   [spec-conventions.md](../docs/spec-conventions.md)'s "no tier-skipping" rule.
   A directory listing proves a file exists; it does not prove the file contains
   the named target. The audit is targeted: the injected diffs/status rows are
   first-class evidence, and the prompt must not tell the agent to perform
   blanket full-file reads across every changed spec.
3. Create implementation beads only under the injected `work_epic`, and
   label/bond each bead to the spec(s) it implements. Beads outside the work
   epic cannot satisfy `LOOM_TODO` validation.

A successful `loom todo` session has exactly one success outcome: emit
`LOOM_TODO: <json>` on the final line. The JSON must carry a non-empty final
work-epic title and report every changed spec exactly once, with
`Decomposed { beads }` for specs that produced non-empty work and
`NoWork { reason }` for specs audited as requiring no implementation change (for
example typo-only spec wording). Audit beads count as `Decomposed` work when
broad missing or stale evidence needs a focused implementation-session
investigation; the bead description names the unresolved evidence rows and
concrete audit/implementation boundary. The agent may not omit changed specs,
report a pending state as success, or use `LOOM_COMPLETE` / `LOOM_NOOP` as todo
success.

Decision-needed or dead-end outcomes use worker self-report markers:

- **Clarify on the work epic.** When coverage cannot be determined by inspection
  — spec ambiguity, conflicting verifier targets, cursor/index inconsistency
  needing human choice, or contestable cache trust — the agent emits
  `LOOM_CLARIFY` with the question and `## Options — …` block persisted to the
  **`loom:todo` work epic's** notes/description per the _Options Format
  Contract_ in [Gate](gate.md). The verdict gate applies `loom:clarify` to that
  work epic; the human resolves via `loom inbox`, and a subsequent `loom todo`
  invocation reuses the matching pending work epic.
- **Blocked on the work epic.** When the agent has no candidate resolutions to
  enumerate, it emits `LOOM_BLOCKED` with a reason explaining why options cannot
  be safely surfaced; the work epic remains non-active and spec cursors do not
  advance.

Per-bead `loom:clarify` is not appropriate in todo because the child beads under
negotiation may not exist yet, or may be exactly the set whose validity is
disputed. The work epic is the session-stable carrier for "this decomposition
batch is paused pending clarification".

**Work-epic-first always.** The driver creates or reuses the `loom:todo` work
epic before rendering `todo.md`, so clarify/block paths always have a valid
target and the agent never has to create the batch container.

**Enumerate-everything defaults are forbidden by data, not by grep.** A fixed
decomposition axis — e.g. "setup, implementation, tests, documentation" applied
across the board irrespective of evidence — is the failure mode this discipline
targets. The combined effect of (i) typed criterion evidence exposing
current/missing/stale verifier state and (ii) the exact-roster `LOOM_TODO`
validator makes such fan-outs structurally unviable. `loom gate review`'s
judge-tier walk catches any decomposition that bypasses the evidence surface to
re-introduce enumerate-everything beads.

**Template-agnostic.** The partial describes the audit obligation in terms of
"changed specs", "criteria in scope", and "representative implementations", not
specific file paths or crate names. Downstream consumers of loom whose workspace
layouts differ from this one inherit the same discipline against their own
layouts.

## Success Criteria

### Task acceptance

- Serde round-trips the typed criterion-reference metadata without copying
  requirement text, verifier bindings, or verdicts. Malformed identifiers fail
  ingestion; externally supplied JSON cannot construct snapshot-resolved
  acceptance without the separate resolution boundary.
  [test?](task_acceptance_metadata_serde_preserves_reference_boundary)

<!-- prettier-ignore -->
- The production todo finalizer resolves agent-proposed assignments against its
  preflight snapshot and accepted tasks, then serializes and writes reference
  metadata through the Rust Beads boundary without replacing unrelated metadata.
  Invalid assignments or binding-write failures prevent activation, cursor
  advancement, and note consumption; prose descriptions cannot substitute for
  the structured proposals. [system?](nix run .#test-quint -- todo-acceptance-persistence)

### Workflow commands

- `loom todo` performs deterministic changed-spec preflight from durable
  spec-epic cursors (`loom.todo_cursor`), Git, and the current `docs/README.md`
  spec index before rendering any agent prompt. It never consults `loom:active`,
  a current-spec cache key, or the LLM to decide the changed-spec set
  [test](todo_preflight_discovers_active_inactive_and_new_specs)

- `loom todo` ensures exactly one `loom:spec spec:<label>` spec epic per indexed
  spec. Missing spec epics are created and make the spec uninitialized/changed;
  duplicate spec epics block with conflicting IDs; missing cursor metadata on an
  existing spec epic blocks with an exact repair diagnostic
  [test](todo_missing_spec_epic_initializes_existing_missing_cursor_blocks)

- `loom todo` creates or reuses one `loom:todo` work epic for the preflight
  changed-spec set and requires a final `LOOM_TODO:` marker whose typed payload
  includes a non-empty final title and covers exactly that set. Generic
  `LOOM_COMPLETE` / `LOOM_NOOP`, missing rows, missing/empty title, malformed
  JSON, nonexistent beads, beads outside the work epic, or extra/omitted specs
  fail validation [test](todo_success_marker_must_cover_exact_changed_spec_set)

- Validated `LOOM_TODO` finalization is all-or-nothing across changed specs:
  every changed spec cursor advances to the preflight HEAD (including `NoWork`
  outcomes), `LOOM_TODO.title` is applied to the work epic, `loom:todo` is
  removed from the work epic, `loom:active` is applied to it, and previous
  active state is cleared. Any failure leaves cursors and active state unchanged
  [test](todo_finalization_advances_cursors_and_active_epic_atomically)

- Missing criterion evidence in `.loom/cache.db` produces typed
  `EvidenceState::Missing` rows in `criterion_status`; it is never treated as no
  criteria or no work. Malformed criteria block preflight
  [test](todo_missing_criterion_cache_rows_are_missing_evidence)

- `loom todo --help` documents deterministic all-spec changed-spec preflight,
  the fail-loud guarantee for blocked/unregistered/stale specs, and that
  successful todo sets the active work epic only after every changed spec is
  represented [test](loom_todo_help_documents_multispec_fail_loud_behavior)

### Verdict gate

- `LOOM_CLARIFY` from a `loom todo` session targets the **`loom:todo` work
  epic** (rationale per
  [Todo — Decomposition Discipline](#decomposition-discipline)); the agent's
  `## Options — …` block is persisted to the work epic's notes per
  [Inbox's Options contract](inbox.md#options-format-contract) before the label
  is applied [test](todo_clarify_marks_work_epic)

### Cache database

- `CacheDb::rebuild` mirrors exactly one `loom:spec spec:<label>` spec epic per
  indexed spec, regardless of epic status; duplicates fail with conflicting IDs
  [test](cache_rebuild_requires_one_spec_epic_per_indexed_spec)

- `loom todo` creates a missing spec epic during preflight, treats the spec as
  uninitialized/changed, and blocks when an existing spec epic lacks
  `loom.todo_cursor` metadata
  [test](todo_missing_spec_epic_initializes_existing_missing_cursor_blocks)

- `loom todo` closes driver-created or already-open spec metadata epics with
  reason `spec metadata carrier`, so spec epics do not remain open solely
  because they carry metadata [test](todo_preflight_closes_spec_metadata_epics)

- `loom todo` rejects malformed, missing, non-ancestor, or unknown
  `loom.todo_cursor` SHAs with diagnostics that name the spec epic and repair
  surface [test](todo_invalid_spec_cursor_blocks_loudly)

- `loom todo` discovers changed specs by comparing each spec/index row at `HEAD`
  against the spec epic's durable cursor; it includes inactive/stale specs and
  brand-new indexed specs regardless of `loom:active`
  [test](todo_preflight_discovers_active_inactive_and_new_specs)

- `loom todo` creates one `loom:todo` work epic with a placeholder title before
  rendering the agent prompt, records `loom.todo_head`, `loom.todo_fingerprint`,
  and changed spec labels on it, and does not add `loom:active` until validation
  succeeds [test](todo_creates_pending_work_epic_before_agent_prompt)

- A pre-existing open `loom:todo` work epic with matching head and
  `TodoFingerprint` is reused; multiple matches or non-matching pending work
  epics block with an Options-format diagnostic
  [test](todo_reuses_matching_pending_work_epic_else_blocks)

- `loom-protocol::todo::parse_todo_success` accepts exactly `LOOM_TODO: <json>`
  final lines and returns typed `TodoSuccess`; malformed JSON, missing fields,
  empty `title`, empty `Decomposed.beads`, empty `NoWork.reason`, or wrong
  prefix fail parse [test](todo_success_marker_parses_to_typed_protocol)

- `loom todo` validates `TodoSuccess.head`, `TodoFingerprint`, work epic id,
  final title, exact changed-spec coverage, bead existence, and bead parentage
  under the work epic before finalization
  [test](todo_success_validation_rejects_missing_extra_or_misparented_beads)

- Validated `NoWork` outcomes advance the spec cursor just like `Decomposed`
  outcomes; no-work rows require a non-empty reason
  [test](todo_no_work_outcome_advances_cursor_with_reason)

- Failed todo validation leaves the work epic labelled `loom:todo`, writes
  diagnostics to it, advances no spec cursor, and does not change `loom:active`
  [test](todo_success_missing_changed_spec_fails_without_advancing)

- Validated or blocked `loom todo` output prints a driver-authored per-spec
  summary covering every changed spec and its outcome; a changed spec missing
  from the summary is a validation failure
  [test](todo_output_summarizes_every_changed_spec_outcome)

- Validated todo finalization removes `loom:todo`, applies the sole
  `loom:active` label to the work epic, clears any previous active epic, and
  advances every changed spec epic's `loom.todo_cursor` to the preflight HEAD
  all-or-nothing [test](todo_finalization_sets_active_and_advances_all_cursors)

- `loom todo` renders implementation notes for each changed spec into the
  relevant work beads and deletes those notes only after the spec cursor
  advances during validated finalization
  [test](todo_consumes_notes_only_after_validated_finalization)

### Integration tests

- A dedicated no-selector Pi fixture stalls after one prompt event so the
  assembled todo path emits its workflow stall warning without adding a stall
  mode to the general mock-pi table
  [test](loom_todo_pi_stall_mid_session_emits_stall_warning)

- Todo validation rejects an agent `LOOM_TODO` payload that omits any changed
  spec; no spec cursor advances and no work epic becomes `loom:active`
  [test](todo_success_missing_changed_spec_fails_without_advancing)

### Decomposition Discipline

- Decomposition treats zero-commit current passing verifier evidence as positive
  coverage evidence instead of blindly creating implementation work.
  [test](todo_template_requires_zero_commit_pass_evidence)

## Requirements

### Command entrypoint

[Acceptance](#success-criteria).

- `loom todo` — deterministic spec-to-beads decomposition. It discovers every
  changed spec from spec epics' durable `loom.todo_cursor` metadata, Git, and
  the current `docs/README.md` spec index — never from `loom:active`,
  current-spec cache state, or the LLM. It creates/ensures spec epics, creates
  or reuses one `loom:todo` work epic, renders the changed-spec roster to the
  todo agent, and accepts success only via a validated `LOOM_TODO:` payload
  carrying a final work-epic title and covering exactly that roster.
  Finalization applies the title, removes `loom:todo`, applies `loom:active`,
  and advances every changed spec cursor to the preflight HEAD all-or-nothing.

### Functional

18. **Decomposition-phase wiring.** `loom todo` runs deterministic changed-spec
    preflight before rendering the prompt, creates or reuses the `loom:todo`
    work epic, and surfaces a per-criterion `CriterionStatus` row (shape owned
    by [Templates](templates.md)) for every changed spec. Criterion evidence is
    read from the unified `.loom/cache.db`; empty cache surfaces as
    `EvidenceState::Missing` rows — staleness is exposed, not papered over. The
    todo agent's only success terminal is `LOOM_TODO: <json>`, parsed by
    `loom-protocol::todo` and validated for a final work-epic title plus the
    preflight roster. `LOOM_CLARIFY` from a todo session targets the `loom:todo`
    work epic because the child beads under negotiation may not yet exist.

### Functional

17. **Decomposition discipline in `todo`.**
    `partial/decomposition_discipline.md`, pinned in `todo` only, requires the
    decomposition agent to decompose the driver-injected changed-spec roster
    exactly, confirm missing work by consulting `criterion_status` and
    representative implementations before authoring non-audit beads, create
    beads only under the injected `loom:todo` work epic, and use
    `LOOM_TODO: <json>` as the only success marker. `LOOM_CLARIFY` targets the
    work epic with a `## Options — …` block when coverage cannot be determined.

### Functional

- Todo preflight, terminal validation, and lifecycle tests execute the
  [harness-owned Todo contract](harness.md#functional), including production CLI
  routing where cursor discovery is the behavior under test

## Out of Scope

- Implementation and publication are subsequent Loop/Gate work. Agent-authored
  prose is not authoritative task-binding metadata.

# Spec decomposition

Derives changed-spec work from durable cursors, decomposes it, and finalizes
typed task assignments and work epics.

## Problem Statement

Spec changes must become the right implementation work without duplicating
already-satisfied requirements or trusting stale passes. Todo fixes the changed
roster, guides evidence-led decomposition, and finalizes explicit task bindings.

## Architecture

Deterministic preflight supplies the roster and coverage; the agent proposes
work, and the driver resolves and persists the accepted handoff. Related owners:
[specs](specs.md), [plan](plan.md), [loop](loop.md), [harness](harness.md),
[templates](templates.md), [protocol](protocol.md), [inbox](inbox.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Binding proposal and persistence

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
parented beneath it. Creation records `loom.todo_state=uninitialized` without
inventing a cursor. That explicit state survives interruption, including before
closing the metadata epic, and remains changed on retry. Successful finalization
replaces it with the cursor and removes the initialization state. A missing
cursor without this state, malformed state, or contradictory state plus cursor
is an error, not an invitation to restart discovery from an invented baseline.

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

### Bounded evidence delivery and resumption

[Acceptance](#bounded-context-and-retry).

The entry prompt, also the fixed portion of compaction re-pinning, retains the
complete roster, batch identity, workflow rules, and an explicit evidence index,
not every raw diff and criterion. The index addresses complete per-spec
criterion/status, note, and diff sections in bounded UTF-8 pages. Splitting
preserves exact content, including multiline verifier commands; no criterion or
note is silently truncated or omitted to fit. Oversized entry/index context
fails before spawn with an actionable diagnostic rather than dispatching an
incomplete batch. Byte and line bounds govern delivery, not a claim about every
model's token capacity or a new way to select fewer specs.

The agent inspects every owner's criterion and note pages, consulting diff pages
and representative source selectively. It works one topic at a time, merges
concise progress into the existing work epic's description, and inspects that
epic and all existing child drafts before creating more work. Checkpoints
preserve prior human decisions and are hints to recheck, not trusted acceptance,
cursor advancement, or partial success. Scratch pages are recreated on retry;
Beads drafts survive it. Finalization still requires the complete fixed roster.

### Todo Success Marker

[Acceptance](#success-criteria).

`partial/todo_success.md` presents the Todo-owned success payload as Protocol's
`Message::Todo(TodoSuccess)` terminal. It follows the common logical-message
framing, including valid pretty-printed JSON; the compact schema illustration
is:

```text
LOOM_TODO: {"head":"<sha>","fingerprint":"<fingerprint>","work_epic":"<bead-id>","title":"<final work epic title>","specs":[...]}
```

The JSON shape is derived from `loom-protocol::todo::TodoSuccess` as specified
in [Todo — Spec and Work Epic Lifecycle](#spec-and-work-epic-lifecycle). The
template tells the agent to include a required non-empty final work-epic title
plus exactly the changed specs the driver injected, using `Decomposed { beads }`
for non-empty work and `NoWork { reason }` for an audited no-implementation
outcome. `Blocked`, `pending`, or omitted specs are not success rows. The agent
reports dedicated decisions through nonterminal `Clarify` records and ends with
`Waiting` only when decomposition itself has real unresolved prerequisites;
`Retry` and `Blocked` retain their distinct terminal meanings.

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

1. Consult [Specs' criterion-status surface](specs.md#criterion-status-surface)
   for each criterion in each changed spec.
   [Current coverage](evidence.md#coverage-projection) is distinct from an
   observed pass. A matching annotation or `commits_since: 0` does not establish
   admissibility; inspect the supplied coverage projection and retained
   blockers. `Missing`, `StaleAnnotation`, or unavailable admission never means
   the criterion is complete. Reuse-disabled results do not themselves prove
   missing implementation or a permanent criterion failure.

2. Read representative existing implementations and verifier functions for
   criteria where coverage is not established, evidence is failed or skipped, a
   retained blocker applies, or the agent judges the verifier target may not
   exercise the live system per
   [spec-conventions.md](../docs/spec-conventions.md)'s "no tier-skipping" rule.
   A directory listing proves a file exists; it does not prove the file contains
   the named target. The audit is targeted: the injected diffs/status rows are
   first-class evidence, and the prompt must not tell the agent to perform
   blanket full-file reads across every changed spec.
3. Create implementation beads only under the injected `work_epic`, and
   label/bond each bead to the spec(s) it implements. Beads outside the work
   epic cannot satisfy `LOOM_TODO` validation.

A successful `loom todo` session has exactly one success outcome: the shared
Protocol decoder admits a final `Todo` terminal. The JSON must carry a non-empty
final work-epic title and report every changed spec exactly once, with
`Decomposed { beads }` for specs that produced non-empty work and
`NoWork { reason }` for specs audited as requiring no implementation change (for
example typo-only spec wording). Audit beads count as `Decomposed` work when
broad missing or stale evidence needs a focused implementation-session
investigation; the bead description names the unresolved evidence rows and
concrete audit/implementation boundary. The agent may not omit changed specs,
report a pending state as success, or use `LOOM_COMPLETE` / `LOOM_NOOP` as todo
success.

Decision discovery and decomposition outcome are separate:

- The agent persists one dedicated child decision bead per currently discovered
  unresolved choice under the injected work epic, with an Inbox Options brief,
  and reports the full known set through nonterminal `Clarify` records.
  [Loop's batch admission](loop.md#decision-batches-and-attributed-waits) stages
  candidates outside dispatch/queues, validates the complete reference graph,
  and records each brief's admitted or blocked-repair disposition. It does not
  label the work epic itself `loom:clarify` or duplicate the briefs onto it.
- Decisions needed to complete decomposition block the work epic through actual
  dependency edges, followed by an admitted `Waiting` terminal. Decisions needed
  only for particular implementation work block those tasks, not the entire
  decomposition batch. A complete valid roster can finalize with `Todo` even
  while such implementation decisions remain unresolved.
- A genuine semantic dead end ends with `Blocked` and a typed reason explaining
  why safe options cannot be framed. The work epic remains non-active and
  cursors do not advance; a bare `Clarify` record is never a replacement
  terminal.

Driver-recognized dedicated decision children, including malformed-brief repair
items, are not implementation tasks and do not satisfy `Decomposed.beads`,
task-binding proposals, or criterion coverage. Recognition requires contextual
reference/graph admission, not an agent label or Options-looking text.
Unreported/unaccounted candidate children prevent finalization rather than
silently becoming work or disappearing from the roster. The implementation
handoff still accounts for every changed spec; recognized decisions retain their
own durable queue dispositions.

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

### Decision wait and resumption

[Acceptance](#decision-scheduling).

Todo admits the shared unit `Waiting` terminal only for a non-closed pending
work epic with active declared prerequisites that genuinely prevent completing
the fixed decomposition. [Loop](loop.md#decision-batches-and-attributed-waits)
owns attributed blocked status and release; no source Options copy or
human-queue label is required. Waiting is not partial `Todo` success: the work
epic remains `loom:todo`, inactive, with cursors and implementation notes
unchanged. Existing drafts, checkpoints, decision IDs, edges, and batch identity
survive.

A later preflight rechecks attributed waits before dispatch. While real
prerequisites remain, it reports the same pending wait without spawning another
agent to rediscover those decisions. After release it reuses the matching
head/fingerprint work epic and existing children, revalidates current evidence
and bindings, and completes the full roster under the ordinary finalizer.
Unrelated holds and head/fingerprint mismatches retain their existing fail-loud
behavior; dependency closure does not authorize stale finalization or automatic
cursor advancement.

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
  existing epic without explicit uninitialized state blocks with an exact repair
  diagnostic
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

- Todo decision reporting admits dedicated decision children under the pending
  work epic without copying briefs or applying `loom:clarify` to that epic; the
  final outcome independently reflects whether decomposition can finish.
  [test?](todo_decision_records_are_independent_of_decomposition_terminal)

### Decision scheduling

- Todo waiting preserves the pending inactive work epic, fixed batch identity,
  drafts, decision state, notes, and cursors without partial finalization or
  retry-budget consumption.
  [test?](todo_waiting_preserves_pending_batch_without_finalization)

- Pending preflight skips rediscovery dispatch while actual prerequisites remain
  and resumes the matching released batch without duplicate children or lost
  human decisions; current mismatches and independent holds still block.
  [test?](todo_preflight_resumes_attributed_waits_without_rediscovery)

- Todo finalization excludes contextually recognized decision children,
  including malformed-brief repair items, from implementation-roster and binding
  validation; labels/prose cannot hide unaccounted candidates or ordinary
  drafts. Exact changed-spec coverage remains required. Decisions blocking only
  implementation do not prevent an otherwise valid final handoff.
  [test?](todo_finalization_distinguishes_decisions_from_implementation_roster)

### Cache database

- `CacheDb::rebuild` mirrors exactly one `loom:spec spec:<label>` spec epic per
  indexed spec, regardless of epic status; duplicates fail with conflicting IDs
  [test](cache_rebuild_requires_one_spec_epic_per_indexed_spec)

- `loom todo` creates a missing spec epic during preflight, treats the spec as
  uninitialized/changed, and blocks when an existing spec epic lacks both a
  `loom.todo_cursor` and explicit uninitialized metadata
  [test](todo_missing_spec_epic_initializes_existing_missing_cursor_blocks)

- `loom todo` closes driver-created or already-open spec metadata epics with
  reason `spec metadata carrier`, so spec epics do not remain open solely
  because they carry metadata [test](todo_preflight_closes_spec_metadata_epics)

- `loom todo` rejects malformed, non-ancestor, or unknown `loom.todo_cursor`
  SHAs with diagnostics that name the spec epic and repair surface
  [test](todo_invalid_spec_cursor_blocks_loudly)

- `loom todo` discovers changed specs by comparing each spec/index row at `HEAD`
  against the spec epic's durable cursor; it includes inactive/stale specs and
  brand-new indexed specs regardless of `loom:active`
  [test](todo_preflight_discovers_active_inactive_and_new_specs)

- `loom todo` creates one `loom:todo` work epic with a placeholder title before
  rendering the agent prompt, records `loom.todo_head`, `loom.todo_fingerprint`,
  and changed spec labels on it, and does not add `loom:active` until validation
  succeeds [test](todo_creates_pending_work_epic_before_agent_prompt)

- A matching pending `loom:todo` work epic is reused across attributed waiting
  and release; multiple matches or non-matching head/fingerprint still block
  with corrective context rather than creating another batch.
  [test?](todo_reuses_matching_pending_work_epic_across_dependency_wait)

- The shared decoder returns `Message::Todo(TodoSuccess)` with the domain's
  required title, nonempty decomposed tasks, and nonblank no-work reasons;
  malformed payloads fail before contextual finalization. Domain entry points
  delegate framing to Protocol, not a final-line-only parser.
  [test?](todo_success_payload_uses_shared_logical_message_decoder)

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

### Bounded context and retry

- Interrupted initialization reuses the same explicitly uninitialized spec and
  work epics without advancing a cursor. Finalization writes the actual
  preflight cursor and removes the initialization state; a subsequent unchanged
  preflight has no new work.
  [test](todo_retries_uninitialized_specs_without_advancing_cursors)

- Interruption after spec-epic creation but before closing it leaves an explicit
  uninitialized record that retry closes and reuses without duplicate creation.
  [test](todo_retries_after_spec_creation_before_close)

- A failed finalization compensates a new spec back to its explicit
  uninitialized state, preserving retry without inventing a cursor.
  [test](todo_failed_finalization_restores_uninitialized_cursor_state)

- Malformed or contradictory initialization metadata is rejected before
  dispatch, rather than treating damaged finalized state as a new spec.
  [test](todo_rejects_contradictory_initialization_metadata)

- Production preflight handles a twenty-owner, thousand-plus-criterion batch
  with a bounded fixed entry prompt for dispatch/re-pin and complete per-spec
  evidence pages. Retrying recreates those pages without duplicate epics or lost
  notes, drafts, or checkpoints; an incomplete final roster remains invalid.
  [test](todo_large_batch_pages_all_evidence_and_resumes_fixed_roster)

- Evidence pages preserve long Unicode rows, CRLF, and exact quoted command
  bytes when reconstructed in order.
  [test](pages_preserve_long_unicode_rows_and_exact_command_bytes)

- Evidence pages bound short-line counts as well as bytes, so many short lines
  cannot defeat bounded disclosure. [test](pages_also_bound_many_short_lines)

- Exceeding the entry/index size bound is an explicit error, not truncated
  context accepted as complete. Production rejects oversized pinned context
  before dispatch while retaining the same retryable batch and untouched
  cursors.
  [test](todo_oversized_context_blocks_before_dispatch_without_losing_retry_state)

### Integration tests

- A dedicated no-selector Pi fixture stalls after one prompt event so the
  assembled todo path emits its workflow stall warning without adding a stall
  mode to the general mock-pi table
  [test](loom_todo_pi_stall_mid_session_emits_stall_warning)

- Todo validation rejects an agent `LOOM_TODO` payload that omits any changed
  spec; no spec cursor advances and no work epic becomes `loom:active`
  [test](todo_success_missing_changed_spec_fails_without_advancing)

### Decomposition Discipline

- Rendered decomposition guidance distinguishes admissible coverage from
  historical passes, requires attention to retained blockers and unavailable
  admission, and avoids both cache-driven no-work claims and automatic
  implementation tasks merely because result reuse is disabled.
  [test?](todo_template_distinguishes_current_coverage_from_historical_results)

<!-- prettier-ignore -->
- Actual todo preflight and prompt construction supply current coverage and
  retained blockers alongside historical criterion observations. Disqualifying
  same-commit trust/history changes cannot leave an unqualified coverage pass
  in the agent's context. Inspection does not rerun verifiers or grant publication authority,
  and missing evidence still permits targeted audit rather than blind fan-out.
  [system?](nix run .#test-quint -- coverage-projection)

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

1. **Decomposition-phase wiring.** `loom todo` runs deterministic changed-spec
   preflight before rendering the prompt, creates or reuses the `loom:todo` work
   epic, and surfaces a per-criterion `CriterionStatus` row (shape owned by
   [Specs](specs.md#criterion-status-surface)) for every changed spec.
   Historical observations come from `.loom/cache.db`; empty cache surfaces as
   `EvidenceState::Missing`. Evidence's current coverage projection accompanies
   those rows, exposing inadmissibility and retained blockers without treating a
   cached pass as authority. The todo agent's only success terminal is
   Protocol's `Todo(TodoSuccess)`, validated for a final title and the complete
   preflight roster. Dedicated decisions and genuinely paused decomposition
   follow [decision waiting](#decision-wait-and-resumption); they do not change
   the success roster or trusted binding-persistence boundary.

2. **Decomposition discipline in `todo`.**
   `partial/decomposition_discipline.md`, pinned in `todo` only, requires the
   decomposition agent to decompose the driver-injected changed-spec roster
   exactly, confirm missing work by consulting `criterion_status` and
   representative implementations before authoring non-audit beads, create beads
   only under the injected `loom:todo` work epic, and use `LOOM_TODO: <json>` as
   the only success marker. Decision reporting, actual prerequisite edges, and
   Todo waiting follow [Decomposition Discipline](#decomposition-discipline),
   rather than using the work epic as a multi-question Options carrier.

### Lifecycle coverage

- Todo preflight, terminal validation, and lifecycle tests execute the
  [Todo lifecycle contract](#spec-and-work-epic-lifecycle), including production
  CLI routing where cursor discovery is the behavior under test

## Out of Scope

- Implementation and publication are subsequent Loop/Gate work. Agent-authored
  prose is not authoritative task-binding metadata.

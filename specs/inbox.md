# Human resolution

Presents human decisions and diagnostics, resolves clarify and blocked work, and
hands approved tune batches to the trusted driver.

## Problem Statement

Agents need a clear way to request human decisions without treating proposed
options as executable authority. Inbox presents the queue and decision context,
then separates conversational resolution from trusted tune application.

## Architecture

The human reviews typed queue items in chat; accepted tune proposals cross a
separate driver apply boundary. Related owners: [findings](findings.md),
[tuning](tuning.md), [agent](agent.md), [gate](gate.md), [loop](loop.md),
[protocol](protocol.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Inbox Modes

[Acceptance](#success-criteria).

`loom inbox` is the cross-spec human queue for non-closed `loom:clarify`,
`loom:blocked`, and `loom:infra` beads plus pending, blocked, and apply-failed
tune proposals. List and view run on the host without mutation; chat launches an
interactive backend.

| Mode             | Invocation                         |
| ---------------- | ---------------------------------- |
| List             | `loom inbox` / `loom inbox list`   |
| View by number   | `loom inbox view <N>`              |
| View by bead     | `loom inbox view -b <id>`          |
| View by proposal | `loom inbox view -p <proposal-id>` |
| Chat queue       | `loom inbox chat`                  |
| Chat by number   | `loom inbox chat <N>`              |
| Chat by bead     | `loom inbox chat -b <id>`          |
| Chat by proposal | `loom inbox chat -p <proposal-id>` |

| Flag                                          | Purpose                     |
| --------------------------------------------- | --------------------------- |
| `-s` / `--spec <label>`                       | Filter to a spec.           |
| `-k` / `--kind clarify\|blocked\|infra\|tune` | Filter to one item kind.    |
| `-b` / `--bead <id>`                          | Address a bead-backed item. |
| `-p` / `--proposal <id>`                      | Address a tune proposal.    |

Filters apply before stable kind/FIFO ordering. View renders durable options,
diagnostics, and repair paths.

Inbox chat has human-authorized Beads write access to queued items and explicit
linked-work hold/cancellation actions within their validated decision context.
This preserves source actions formerly authorized on source-hosted briefs,
without granting unrelated-work authority. It may repair tune artifacts only in
the proposal checkout. Human decision content and resolution remain
authoritative. The driver reconciles only explicitly attributed scheduling state
under [orchestration ownership](#orchestration-ownership), not a generic worker
verdict. Protocol's `Complete` ends chat without tune application; `Apply`
requests one validated all-or-nothing tune proposal batch through integration,
gate, and push. A failed batch publishes nothing and leaves every proposal in
`apply_failed` for explicit later review.

There is no host-side pick, reply, resolve, dismiss, or apply mutation surface.
Options are discussion context for chat rather than executable menu entries.

### Orchestration ownership

Acceptance: [decision resolution](#decision-resolution) and
[human-state preservation](#workflow-commands).

The human/chat agent owns decision content, authorized outcome recording,
decision closure, and independent holds. The driver owns only the scheduling
waits it explicitly admitted and attributed under
[Loop](loop.md#decision-batches-and-attributed-waits). It may deterministically
release those waits after chat by re-reading actual decision/dependency state,
without interpreting prose or using `Complete` as an instruction to unblock.

This separation forbids generic canonical unblocking, status reversion, and
label re-application that would undo human changes. A human adoption of an
independent hold cancels/replaces the driver wait attribution; the driver does
not infer ownership from `status=blocked` alone. Existing semantic, infra,
deferred, and closed state remain untouched. Interrupted chat can leave valid
human writes durable; scheduling rechecks do not retry chat or fabricate a
missing human answer. List/view remain read-only.

### Decision beads and resolution

[Acceptance](#decision-resolution).

Each admitted dedicated decision bead carries one canonical Options brief and
enters the ordinary `loom:clarify` queue. Direct worker and driver-origin
decisions follow
[Loop's batch admission](loop.md#decision-batches-and-attributed-waits);
review-origin decisions follow Findings' existing scoped materialization. Staged
candidates are not queue items; malformed briefs appear as blocked repair items
on the defective decision, not on its discovering source. Discovery parentage
does not imply an execution prerequisite, and implementation beads waiting on
decisions do not become duplicate human-queue items.

All admitted currently discovered decisions are available in one chat queue,
subject to the user's explicit filters. The human may resolve them
conversationally one at a time, stop after a subset, or leave items unresolved.
No worker rerun is needed merely to expose the next already-discovered question.
The canonical Options summary supplies the question/list summary; it is not
scraped from adjacent agent stdout or copied into protocol payloads.

The authorized resolution records the human outcome and closes its dedicated
decision bead. Unanswered decisions remain queued. Decision closure alone means
intent was resolved, not affected implementation finished or the source
abandoned; required implementation remains separately bound work. A distinct
explicit human instruction may authorize a linked source hold or cancellation
within the validated decision context, preserving the existing abandon-the-bead
option. Such an action is not automatic decision resolution or positive
implementation coverage. Partial answers release only work whose actual
prerequisites are satisfied, through Loop's attributed wait resumption.

## Success Criteria

### Workflow commands

- Bare `loom inbox` / `loom inbox list` lists every outstanding non-closed bead
  carrying `loom:blocked`, `loom:clarify`, or `loom:infra` across all specs plus
  pending, blocked, and apply-failed tune proposal beads (cross-spec default);
  no active-spec cache value is consulted, and closed beads are excluded even
  when labels remain [test](inbox_list_includes_infra_and_excludes_closed_items)

- `loom inbox list -s <label>` (alias `--spec`) filters the list to items
  carrying the `spec:<label>` bead label or proposal metadata
  [test](inbox_spec_filter_narrows_list_to_matching_spec)

- `loom inbox list -k clarify|blocked|infra|tune` filters by exclusive item
  kind; absence of `--kind` means all kinds. Filters narrow before positional
  numbering and default ordering is group-first (`clarify`, `blocked`, `infra`,
  `tune`) then FIFO within each group
  [test](inbox_kind_filter_narrows_list_including_infra)

- `loom inbox view <N>` / `loom inbox view -b <id>` /
  `loom inbox view -p <proposal-id>` renders the addressed item host-side
  without launching a container, including durable ids, infra diagnostic fields
  when present, and manual repair paths; corrupt/unavailable tune proposals
  remain tune-kind items with blocked status rather than being skipped
  [test](inbox_view_modes_render_host_side_with_infra_diagnostics)

- `loom inbox` exposes no host-side `pick`, `reply`, `resolve`, `apply`,
  `--option`, `--text`, `-c/--chat`, or `-d/--dismiss`; conflicting address
  flags error before any side effects
  [test](inbox_removed_flags_and_address_exclusivity)

- `loom inbox chat`, `loom inbox chat <N>`, `loom inbox chat -b <id>`, and
  `loom inbox chat -p <proposal-id>` launch an interactive session in a
  container using the `inbox.md` template; list/view stay host-side
  [test](loom_inbox_chat_launches_container)

- Chat has full bd-write authority on queued bead-backed items and separately
  human-authorized linked-work holds/cancellations in the validated decision
  context, not unrelated work. Tune artifact repair stays under
  `.loom/tune/<id>/repo/`; chat never pushes or leaves `.loom/integration`
  dirty.
  [test?](inbox_chat_authority_preserves_explicit_linked_work_resolution_scope)

- Inbox preserves human-owned outcomes and holds after chat, without generic
  canonical unblocking, status reversion, or label restoration. Deterministic
  scheduling release requires matching driver wait attribution rather than an
  interactive completion marker.
  [test?](inbox_driver_preserves_human_state_with_attributed_wait_release)

- `LOOM_COMPLETE` from inbox exits cleanly with no driver-side apply;
  `LOOM_APPLY: {"proposals":[...]}` validates accepted tune proposal ids and
  triggers one end-of-chat driver apply batch. `LOOM_APPLY` is the sole terminal
  marker for that session, never paired with `LOOM_COMPLETE`
  [test](inbox_apply_marker_triggers_single_driver_handoff)

- The end-of-chat tune apply batch is all-or-nothing: `cherry_pick_conflict`,
  `verify_failed`, `review_failed`, or `push_failed` aborts the batch, pushes
  nothing, leaves `.loom/integration` clean, and marks every proposal in the
  batch `apply_failed` with shared diagnostics
  [test](inbox_apply_batch_is_all_or_nothing)

- `apply_failed` tune proposals appear in the next default inbox and are not
  retried automatically; a later chat must explicitly repair/reauthorize a
  subset or reject/regenerate them
  [test](apply_failed_tune_proposals_require_reauthorization)

- Interactive-session crashes (container OOM, observer abort, swallowed marker)
  exit non-zero with a diagnostic; the driver does NOT auto-retry
  [test](loom_inbox_chat_crash_exits_nonzero_without_auto_retry)

- `loom inbox chat` with `-s <label>` and/or `-k <kind>` scopes the chat queue;
  without filters, the session sees every outstanding human decision item
  regardless of active work epic and normally works them one at a time
  [test](loom_inbox_chat_scope_filters_queue)

### Decision resolution

- Each decision has one unambiguous active Options brief across notes and
  description, with a nonblank summary and at least one numbered, titled Option
  subsection. The canonical summary supplies the human question/list summary;
  missing, malformed, or ambiguous briefs cannot be admitted as valid direct
  clarification context.
  [test](canonical_options_brief_is_unique_and_supplies_decision_summary)

- One Inbox session exposes the complete admitted decision batch under the
  selected queue filters, supports conversational partial resolution, and leaves
  unanswered decisions available without another worker discovery run.
  [test?](inbox_session_exposes_all_admitted_decisions_and_retains_unanswered_items)

- Authorized dedicated-decision resolution records the outcome and closes the
  decision, not automatically affected implementation or its discovering source.
  Any explicit linked-work hold/cancellation requires separate human authority
  and is not implementation coverage; existing-bead promotion removes its active
  Options block with the label in the authorized resolution update.
  [test?](inbox_resolution_distinguishes_decision_closure_from_implementation)

- Inbox chat exit rechecks only driver-attributed waits against current
  prerequisites, including after partial resolution or interrupted chat, while
  list/view stay read-only and chat is never automatically retried.
  [test?](inbox_chat_exit_rechecks_only_attributed_dependency_waits)

### Integration tests

- The dedicated Pi inbox-bridge follow-up fixture stays outside the mock-pi mode
  table and covers only the probe → prompt → one human follow-up prompt →
  terminal-marker exchange
  [test](inbox_bridge_pi_followup_fixture_accepts_one_prompt_reply)

### Acceptance

- Pending and blocked tune proposal records enter the authoritative
  [Inbox — Inbox Modes](#inbox-modes) flow as tune-kind items, and authorized
  adoption is performed only by that flow's trusted apply handoff
  [test](inbox_apply_marker_triggers_single_driver_handoff)

## Requirements

### Command entrypoint

[Acceptance](#success-criteria).

- `loom inbox` — human decision and operator diagnostic queue for clarifies,
  semantic blocked beads, infra diagnostics, and tune proposals. Bare
  `loom inbox` and `loom inbox list` are read-only; `loom inbox view` renders a
  numbered, bead-addressed, or proposal-addressed item; and `loom inbox chat`
  launches the interactive resolution agent. There is no host-side
  pick/reply/resolve/apply path in v1.

#### Options Format Contract

[Acceptance](#success-criteria).

A bead admitted as `loom:clarify` presents one decision and its candidate paths
in a canonical Markdown block readable by `loom inbox view` and chat. Reviewers
supply evidence to trusted Findings materialization; direct workers author
dedicated decision beads before batch admission. The block is structured
resolution context, not an executable picker:

```markdown
## Options — <one-line summary of the decision>

### Option 1 — Preserve the invariant
<body explaining what reworking the change to preserve the invariant
would look like, including the cost>

### Option 2 — Keep the change on top of the invariant
<body explaining what carrying the contradiction would entail —
which spec section to record the debt in, what cleanup follow-up
to file>

### Option 3 — Change the invariant
<body explaining what updating the spec would entail — which
invariant to weaken or remove, what code realignment would follow>
```

`loom inbox` consumes this format without a host-side option picker:

- **List mode** (`loom inbox`): the `## Options — <summary>` line is rendered as
  the bead's SUMMARY column.
- **View mode** (`loom inbox view <N>` / `loom inbox view -b <id>`): the full
  block is rendered to the user with each `### Option N` heading.
- **Chat mode** (`loom inbox chat`): the block is rendered as structured context
  for the human and chat agent. The chat agent records any authorized decision
  through Beads; Loom does not infer executable actions from option prose.

A clarify bead can present fewer or differently-framed options when the decision
warrants — the format is `### Option <integer> — <title>` for any integer ≥ 1.
The summary line is always required.

**Application paths.** Each path uses one unambiguous `## Options — <summary>`
heading with at least one `### Option <N> — <title>` subsection across bead
notes and description. The writer and admission authority differ:

| Path                         | Brief writer/location                                                                                             | Admission and failure                                                                                                                                                      |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Finding materialization      | Reviewer embeds the brief in finding evidence; trusted Findings act paths persist it on a dedicated decision bead | Findings validates before human-queue materialization; absent/malformed brief falls back to `loom:blocked`, cause `clarify-without-options`                                |
| Direct worker decision batch | Loop/Todo worker persists one brief on each dedicated decision bead and reports its ID                            | Loop/Todo resolves the complete reported set and context before queue admission; absent/malformed brief retains `clarify-without-options`, invalid references fail visibly |
| Driver-origin escalation     | Existing conflict/integrity producer writes the full brief on each dedicated decision bead                        | Same contextual dedicated-decision admission and per-item fallback; the original source/epic receives no duplicate brief or human-queue label                              |
| Existing-bead promotion      | Human-authorized chat agent adds the brief to an existing semantic blocked item                                   | Human-authoritative authorized update; no post-chat driver reinterpretation of the decision                                                                                |

Worker/driver/Findings admission prevents an absent brief from being treated as
a valid clarification. An agent unable to frame safe options reports a semantic
blocked reason instead. Existing-bead promotion remains human-authoritative;
this does not authorize the driver to infer executable actions from its prose.

**The gate does not scrape free-form stdout for `## Options` / `### Option N`
blocks.** Only the structured locations above carry the canonical contract —
`evidence` for mint-from-finding, bead notes/description for worker/driver
dedicated decisions and existing-bead paths. Review clarifications use the
mint-from-finding path; review prompts do not direct agents to mutate bd state.

##### Resolution lifecycle

[Acceptance](#success-criteria).

On a surviving promoted bead, clearing `loom:clarify` removes its active Options
block from notes/description in the same authorized resolution update that
records the human decision. Dedicated decision closure instead retires the whole
item from the active queue; its brief may remain as historical context.

A surviving promoted bead can receive successive clarifications, but only one
active brief at a time. Removing the resolved block prevents later questions
from being confused with previous ones.

Dedicated decision beads, whether worker-, driver-, or finding-origin, close
with the recorded resolution; their brief then passes out of the active queue
and may remain as historical context. Several simultaneous decisions use
separate beads, not accumulated active blocks on a Todo work epic.

## Out of Scope

- A separate host-side pick/reply/resolve/apply command is not provided. Tuning
  owns candidate validity; Inbox does not waive Gate admission.

- A standalone `loom inbox apply` command.

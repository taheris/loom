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
[tuning](tuning.md), [agent](agent.md), [gate](gate.md).

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

Inbox chat has human-authorized Beads write access to queued items and may
repair tune artifacts only in the proposal checkout. The driver does not
reconcile those interactive Beads changes afterward. `LOOM_COMPLETE` ends chat
without host apply; `LOOM_APPLY` requests one validated all-or-nothing tune
proposal batch through integration, gate, and push. A failed batch publishes
nothing and leaves every proposal in `apply_failed` for explicit later review.

There is no host-side pick, reply, resolve, dismiss, or apply mutation surface.
Options are discussion context for chat rather than executable menu entries.

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

- The chat session has full bd-write authority on bead-backed items in its queue
  and may repair tune proposal artifacts only under `.loom/tune/<id>/repo/`; it
  never pushes and never leaves `.loom/integration` dirty
  [test](inbox_chat_bd_authority_and_tune_repair_scope)

- The driver does **not** reconcile bd state after an interactive session — no
  canonical unblock, no status reversion, no label re-application. Whatever
  bd/proposal state the chat agent (with human authorization) established at
  session end IS the state, except for the explicit `LOOM_APPLY` handoff
  [test](inbox_chat_driver_does_not_reconcile_bd_state_after_session)

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

Whenever the gate (or, in practice, the reviewing agent acting on behalf of the
gate) raises `loom:clarify` — for an invariant clash, for a verifier-honesty
concern with multiple resolution paths, or for any review-time decision the user
must pick from — the bead body presents the candidate paths as a structured
markdown block that `loom inbox view` can render and `loom inbox chat` can use
as structured resolution context:

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

**Three application paths, one shape requirement.** Three distinct paths apply
`loom:clarify` to a bead. All three require a well-formed `## Options —
<summary>` heading with at least one `### Option <N> — <title>` subsection
somewhere readable by `loom inbox` (bead notes ∪ description). Each path has its
own writer and validator, but the *shape* of the options block and the *failure
mode* on absence are uniform:

| Path                                                                                                                                                                                                                                    | Writer of the options block                                                                                                | Where the block lives                                                                        | Validator                                                                                                                 | Failure mode                                                                                             |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| **Mint-from-finding** (worker phase emits `LOOM_FINDING` with a clarify-route token)                                                                                                                                                    | Rubric agent — embeds the block inside `evidence`                                                                          | Mint extracts from `evidence` into the minted bead's description                             | `loom gate mint` (per _Deferred remediation processing_ step 4)                                                           | Fall back to `loom:blocked` cause `clarify-without-options`                                              |
| **Direct-emit `LOOM_CLARIFY`** (`loop` / `todo` worker emits the marker; target is the bead under dispatch for `loop`, or the `loom:todo` work epic for `todo` per [Todo — Decomposition Discipline](todo.md#decomposition-discipline)) | The worker agent itself, via `bd update --notes` / `bd update --description` against the target before emitting the marker | The target bead/work epic's notes or description                                             | Verdict gate (per [Loop — Verdict Gate](loop.md#verdict-gate) marker definitions)                                         | Fall back to `loom:blocked` cause `clarify-without-options`                                              |
| **Existing-bead promotion** (chat agent in `loom inbox chat` upgrades a `loom:blocked` bead)                                                                                                                                            | The chat agent, with human authorization                                                                                   | The bead's notes (added via `bd update --notes` before `bd update --add-label=loom:clarify`) | None — the chat agent has bd-write authority and the human authorizes each turn (per [Inbox — Inbox Modes](#inbox-modes)) | n/a (no automatic validation; if the chat agent skips the options write, the human catches it next turn) |

The structural enforcement at the chokepoint is what makes "stranded clarify
bead the chat-drafter cannot resolve" unrepresentable for the two worker-phase
paths — the agent either provides a well-formed options block (clarify applied)
or emits `LOOM_BLOCKED` directly with a reason explaining why options cannot be
enumerated (no clarify ever applied). The existing-bead promotion path is not
subject to the chokepoint because chat is human-authoritative.

**The gate does not scrape free-form stdout for `## Options` / `### Option N`
blocks.** Only the structured locations above carry the canonical contract —
`evidence` for mint-from-finding, bead notes/description for loop/todo
direct-emit and existing-bead paths. Review clarifications use the
mint-from-finding path; review prompts do not direct agents to mutate bd state.

##### Resolution lifecycle

[Acceptance](#success-criteria).

The `## Options — <summary>` block lives on the target bead (in notes or
description, per the path table above) only from emit to resolution. When
`loom:clarify` is cleared by an inbox chat session's
`bd update --remove-label=loom:clarify`, the originating options block is
removed from wherever it lives (notes or description) in the same authorized
resolution update that records the human decision.

A single bead can receive multiple clarifications across its lifetime — notably
a `loom:todo` work epic, which hosts decomposition-phase clarifies emitted by
successive `loom todo` invocations while the same pending fingerprint is being
repaired. Without removal, options blocks accumulate and `loom inbox` lists
become ambiguous about which block belongs to the currently active label.

For clarifies hosted on a **dedicated clarify bead** (created via the
mint-from-finding path above and closed during inbox chat), the removal is moot
— the whole bead is closed and the notes/description pass out of scope with it.
The lifecycle contract is load-bearing for the **existing-bead promotion** path
where the bead survives the resolution.

## Out of Scope

- A separate host-side pick/reply/resolve/apply command is not provided. Tuning
  owns candidate validity; Inbox does not waive Gate admission.

- A standalone `loom inbox apply` command.

# Implementation workflow

Schedules isolated workers, reconciles outcomes, retries bounded failures, and
integrates and publishes only through the shared gate.

## Problem Statement

Worker completion is not proof that changes are safe to integrate or publish.
Loop reconciles task acceptance and outcomes, bounds recovery, and coordinates
isolated work through the shared verification and publication boundaries.

## Architecture

The driver resolves acceptance before dispatch, reconciles the worker outcome,
and performs integration/publication effects only through Gate admission.
Related owners: [workspaces](workspaces.md), [specs](specs.md), [todo](todo.md),
[gate](gate.md), [findings](findings.md), [templates](templates.md),
[protocol](protocol.md), [inbox](inbox.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Task acceptance at dispatch

[Acceptance](#task-acceptance).

Every task producer uses the same driver-owned acceptance boundary. Ordinary
implementation tasks, including manually created or split follow-ups, require
explicit criterion assignments resolved under
[Specs](specs.md#task-acceptance-references). Remediation tasks instead carry
[Findings' resolved goals](findings.md#remediation-task-acceptance). The driver
parses the task's declared purpose and references, resolves them against the
current dispatch snapshot, and supplies the corresponding immutable acceptance
form to Templates. Neither prose, labels, an empty default, nor external JSON
claiming to be resolved authorizes dispatch.

The persisted acceptance-reference payload is one tagged sum: either a nonempty
set of criterion references or a nonempty set of remediation-goal references.
Its typed shape excludes mixed purposes and empty acceptance, rather than
allowing two independent optional fields. Parsing produces reference candidates,
not resolved goals; a tag alone proves neither task purpose nor finding
membership. Only the separate driver resolution constructs immutable dispatch
acceptance. Metadata does not store copied resolved authority.

Missing or invalid ordinary bindings require explicit binding or rebinding
before spawn; they cannot silently become remediation. Missing or invalid
remediation goals likewise block with corrective context. This permits repair of
broken acceptance without relaxing finding resolution or ordinary task binding.
Producer-specific handoffs use existing workflow/Beads boundaries, not a second
queue, ad-hoc IDs, or agent-authored trusted metadata. Pending criteria remain
assignable under Specs' existing rule. Worker terminal reconciliation,
integration verification, and publication admission are unchanged; resolved
goals are instructions, not passing evidence.

### Verdict Gate

[Acceptance](#verdict-gate-1).

Worker output is untrusted until the shared [Protocol](protocol.md) decoder,
phase admission, and mechanical reconciliation succeed. Nonterminal decision
reporting is admitted separately from the source's terminal outcome. Interactive
Plan/Inbox do not undergo worker completion reconciliation;
[Inbox](inbox.md#orchestration-ownership) preserves human decisions while
allowing reconciliation of explicitly attributed driver-owned waits. Review
retains [Findings' pairing contract](findings.md#emit-shape).

For a loop worker, the decoded terminal, bead closure, branch diff, and tree
state produce the following result. After transport completion, apply rows in
order; the first matching row determines the outcome. Protocol/phase and
reference/graph/inventory errors precede terminal-state reconciliation and new
queue/wait effects under
[batch admission](#decision-batches-and-attributed-waits).

| Terminal and state                                                                                                | Result                                                                                     |
| ----------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| invalid protocol/phase session, decision reference graph, or candidate inventory                                  | visible protocol/recovery diagnostic; no new queue/wait effects, completion or integration |
| `Blocked` with its checked nonblank reason                                                                        | semantic block                                                                             |
| `Retry` with its typed reason                                                                                     | bounded worker recovery                                                                    |
| `Waiting`, non-closed source, active declared prerequisite, no independent hold                                   | attributed dependency wait; preserve the workspace/branch and continue ready work          |
| `Waiting` with a closed source, no active prerequisite, or incompatible hold                                      | `invalid-waiting` recovery                                                                 |
| `Complete` or `Noop` with an independent hold or authorized cancellation                                          | refuse stale completion; preserve human state without integration or no-work acceptance    |
| `Complete` or `Noop` with an active prerequisite for its own acceptance                                           | reject completion before integration                                                       |
| `Complete` or `Noop` while the bead is not closed                                                                 | `incomplete-signaling` recovery                                                            |
| `Complete` with an empty diff                                                                                     | `zero-progress` recovery                                                                   |
| `Complete` or `Noop` with a dirty tree                                                                            | `tree-not-clean` recovery                                                                  |
| `Noop` with a non-empty diff                                                                                      | reject the false no-work claim through bounded worker recovery                             |
| `Complete`, own prerequisites resolved, no independent hold/cancellation, closed bead, non-empty diff, clean tree | integrate and verify                                                                       |
| `Noop`, own prerequisites resolved, no independent hold/cancellation, closed bead, empty diff, clean tree         | intentional no-work success                                                                |

`Clarify` is a decision-batch record, not a terminal or evidence that the source
must pause. Its admission follows
[decision batches](#decision-batches-and-attributed-waits). `Retry.reason`
consumes the existing in-session retry budget; `Blocked.reason` routes the
source to semantic inbox resolution. Missing terminals retain `swallowed-marker`
recovery; malformed output carries its typed protocol context. A valid `Waiting`
records driver-owned attribution and visibly parks the source as
`status=blocked`, without `loom:blocked`, `loom:clarify`, or `loom:infra`. It
consumes no retry budget and performs no integration or per-bead gate.
Resumption follows current prerequisites, not a fresh worker run to rediscover
the same decision.

The `Blocked` predicate is mechanical: a checked nonblank reason determines
routing without an LLM call. Whether that reason adequately explains why no safe
options can be framed remains a guidance and
[semantic-review obligation](gate.md#verdict-gate), not a decoder or
verdict-table predicate.

Before accepting `Complete`/`Noop`, re-read the source's current prerequisites
and independent holds or authorized cancellation. A human change made during the
worker session vetoes stale positive completion, including after agent-side
closure. Preserve that human state and report the conflict; do not reopen,
replace labels, or automatically redispatch held/cancelled work as protocol
repair. Closed status alone identifies neither cancellation nor permission to
override a recorded hold. This check precedes integration or intentional no-work
acceptance and does not create another retry budget. Rejected or provisional
closure is not workspace-disposal authority; startup and post-attempt removal
follow [Workspaces' cleanup admission](workspaces.md#cleanup-admission).

Per-bead integration verifies worker signatures, rebases, verifies rewritten
signatures, fast-forwards, and runs
`loom gate verify --diff <pre-integration-head>..HEAD`. A deterministic failure
rolls back the integration head, writes a durable gate log, and returns typed
failure evidence to the same bead's recovery prompt. Per-bead integration does
not run LLM review or mint a push marker.

After all molecule work and promoted remediation drain, the push gate fetches
origin, resolves the actual push range, runs pre-push deterministic checks, runs
LLM review, constructs the gate-owned `GateSuccess` receipt, mints the marker,
and pushes inside one critical section. This supplies the context for Gate's
[single publication-attempt handoff](gate.md#publication-attempt-handoff), not
an independent authorization mechanism. Any changed range invalidates prior
whole-scope authorization and reruns gate planning; unit evidence is assessed
under Gate's admission rules. The driver applies Gate's attempt-ending boundary
to failed or interrupted pushes, including recovery after process loss; a retry
resolves current remote state and enters a new admitted attempt rather than
resuming the old handoff. Successful Git and Beads publication is followed by
inside-out closure of currently eligible ancestor epics under
[completion admission](#completion-admission).

Infrastructure failures remain distinct from semantic worker outcomes. Static
configuration/dispatch faults pause the bead as `loom:infra` without transport
retry. Spawn, handshake, backend transport/RPC-frame decoding, or
premature-stream failures use a separate per-loop infra attempt budget and
round-robin behind other ready work. Exhaustion pauses the bead as `loom:infra`;
a later loop invocation gets a fresh budget. `first_event_seen` distinguishes
pre-stream from interrupted sessions. Malformed agent-output messages in an
otherwise completed session instead use protocol diagnostics and the worker
recovery budget; they are not transport framing failures.

Every driver-detected recovery carries a typed `PreviousFailure`, bounded
attempt count, and durable evidence such as dirty paths, verifier failures,
conflict files, gate log path, or agent retry reason. Recovery exhaustion
preserves the cause in Beads. Remediation work is bonded to its originating
molecule before becoming dispatchable so molecule progress and push refusal see
all unresolved blocked, clarify, deferred, and infra state.

[Protocol — Phase admission](protocol.md#phase-admission) owns the shared
variant/phase matrix and logical terminal framing. Loop owns the contextual
state checks above, not another marker parser. Exit status alone does not
authorize state transitions.

### Loop Outcome Types

[Acceptance](#workflow-commands).

`LoopOutcome` and `GateOutcome` are architecture-bearing types: a successful
loop invocation cannot omit the push-gate result. `LoopOutcome` has no default,
is `must_use`, records processed, waiting, clarified, and blocked counts, and
carries a non-optional `GateOutcome`. Processed, waiting, and blocked counts
summarize admitted worker outcomes. Clarified counts distinct unresolved
decision beads admitted during this invocation, including driver-origin
decisions, not sources or `Clarify` records. Repeated IDs count once;
already-closed historical references do not count. A completed source reporting
five admitted decisions can therefore contribute one processed and five
clarified.

`GateOutcome` has three shapes: `Success(GateSuccess)`, `Fail(GateFail)`, and
`NoGate { beads_processed, reason }`. `GateSuccess` is sealed and constructed
only from the evidence defined in
[Gate — Gate success receipt](gate.md#gate-success-receipt). `NoGate` is limited
to no ready work or an explicitly partial selection. The CLI exits zero for
`Success` and `NoGate`, non-zero for `Fail` or `LoopError`.

### Typed `PreviousFailure`

[Acceptance](#typed-previousfailure-1).

`previous_failure` is a typed tagged enum. The driver populates the right
variant based on the verdict-gate cause classification; the template renders
each variant with distinct framing so the agent sees a cause-appropriate prompt
rather than a one-shape blob.

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
    /// `Finding` per [Findings — Findings and Minting](findings.md#findings-and-minting-1)).
    ReviewConcern { summary: String, findings: Vec<Finding> },

    /// Review walk's terminal signal was malformed or mismatched
    /// with the streamed-findings count. Carries the typed
    /// `BadWalk` variant; see [Loop — Verdict Gate](#verdict-gate)
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
    /// anticipate). The integration was rolled back to
    /// the recorded pre-integration head. Carries the verifier-failure list
    /// and durable gate-log path directly. Review concerns are produced
    /// by the molecule-completion review and route through
    /// `ReviewConcern` or `BadWalk` rather than this variant.
    PostIntegrateFail {
        failures: Vec<VerifierFailure>,
        gate_log_path: PathBuf,
    },

    /// Driver-side rebase of the bead branch onto the integration tip
    /// hit textual conflicts. The next dispatch gets the conflict files
    /// plus the new base SHA for one agent retry; a second conflict
    /// creates or reuses a dedicated driver-authored decision bead;
    /// the source enters an attributed wait, not the human queue.
    IntegrationConflict {
        files: Vec<PathBuf>,
        new_base_sha: GitOid,
    },

    /// Worker phase emitted `LOOM_RETRY` — the agent self-reported
    /// that this attempt could not finish but a fresh dispatch is
    /// likely to succeed (environmental failure: tools failing
    /// mid-session, sandbox/cwd unlinked, transient IO; or agent
    /// self-reset: stuck-but-not-blocked, prompt-context exhausted,
    /// approach abandoned). `reason` is the decoded JSON reason,
    /// not adjacent prose. Distinct from
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
```

### Attempt Counter

[Acceptance](#attempt-counter-1).

`attempt` is the per-bead in-session retry counter, populated by the driver and
rendered by `loop.md`:

- `attempt == 0` on fresh bead dispatch — no retry context, no attempt line in
  the template
- Each in-session retry increments `attempt` (bounded by `[loop] max_retries`,
  default 2)
- Resets to 0 when a new bead is dispatched (fix-up beads carry fresh prompts,
  not retry state from the failing bead)
- **Molecule-level iteration is opaque to the agent** — fix-up beads are
  different prompt contexts, and a counter that spans them would be misleading

When `attempt > 0 && previous_failure.is_some()`, `loop.md` prepends a counter
line: `"Retry attempt {attempt} — previous attempt failed with: …"` followed by
the typed `previous_failure` block.

### Loop completion self-check and self-review

[Acceptance](#worker-self-check).

`loop.md`'s quality-gate block instructs the worker to finish by verifying the
exact injected bead range, not by relying on a working-tree shorthand. The
rendered command names `loom gate verify --diff <bead-base>..HEAD` (or
`@{u}..HEAD` only when the upstream is the injected base), and tells the agent
to rerun the self-check after any later commit or hook-generated file change.
The prompt also requires a structured self-review before the final marker:
re-read the bead's criteria, inspect the final diff, check style/spec fit, and
either fix the issue or emit the appropriate worker self-report marker. This is
prompt-level feedback discipline; the driver-side trust boundary remains the
[post-integration workflow](#worker-and-per-bead-integration-checks) and
[Gate's push receipt](gate.md#gate-success-receipt).

### Decision batches and attributed waits

Acceptance: [scheduling](#decision-scheduling),
[batch/outcome admission](#verdict-gate-1), and
[producer acceptance](#task-acceptance).

Workers discover and persist all currently known unresolved decisions before
reporting them through Protocol's `Clarify` records. Each decision is a separate
bead with one [Inbox Options brief](inbox.md#options-format-contract), directly
parented under its discovering task or Todo work epic. Beads-allocated IDs and
explicit relationships are authoritative; dotted ID spelling is not. Workers own
decision content, bead creation, and proposed dependency edges.

**Staging.** Create each new candidate with `status=blocked` and no
`loom:clarify`, `loom:blocked`, or `loom:infra` queue label. Persist untrusted
producer/source and work-root provenance with its brief and proposed edges.
Candidates cannot become implementation work merely because admission has not
finished: Loop excludes them from worker dispatch and Todo refuses to finalize
unaccounted candidate children. Interrupted producers preserve candidates for
validated reuse or repair, not automatic queue admission or guessed bindings.
Candidate provenance is a proposal, never trusted acceptance or wait authority.

**Admission.** Aggregate the complete reported set and resolve every ID's
purpose, parent/work-root context, and actual affected-work graph before new
queue or scheduling effects.

Compare that set and durable prior admission/repair dispositions with the
current persisted candidate inventory attributable to this producer and
work-root before new queue/wait effects or terminal-state reconciliation. Every
owned candidate must be accounted for: an omitted staged candidate rejects
`Complete`/`Noop` before integration even when it affects only other work or the
source was agent-closed. The same check prevents `Waiting` from parking work
behind an unreported, invisible decision. Preserve candidates and report exact
missing IDs for bounded protocol repair, without guessing queue admission or
silently treating them as ordinary implementation tasks. Ambiguous ownership
fails visibly. Valid prior dispositions and historical closed decisions remain
accounted for without requiring redundant emissions. This checks persisted
discoveries, not whether the agent discovered every possible ambiguity.

New candidates must belong to the reporting producer; already admitted decisions
may be reused only in a compatible recorded work-root/dependency context.
Missing IDs, wrong purpose/provenance, ambiguous references, and invalid graphs
fail the handoff with complete diagnostics; valid siblings are retained as
context, not silently admitted from that failed set. Preserve existing human
state and producer-written candidates/edges on failure.

After reference/graph admission, validate each unresolved decision's unique
active brief. Valid briefs enter `loom:clarify`; missing, malformed, or
ambiguous briefs instead put that decision in `loom:blocked` with
`clarify-without-options`, while valid siblings still enter the clarification
queue. The fallback targets the defective decision, never a duplicate brief or
semantic block on its discovering source. Record each disposition and validated
provenance durably so interruption/replay cannot duplicate or erase queue items.
The source's terminal is reconciled independently against its actual
obligations; a brief downgrade alone does not force that source to wait.

Already-closed references to previously admitted decisions retain their recorded
outcomes and historical context. They create no active queue item, are not
reopened, and need no new active brief. This is checked idempotent replay, not
silent removal of invalid references. Admission never scrapes adjacent stdout
for a question.

**Driver-origin decisions.** Existing conflict and integrity producers use the
same dedicated-bead admission and scheduling lifecycle without fabricating an
agent `Clarify` record. Workspaces retains driver-authored integration-conflict
briefs and the single automatic agent retry before escalation. Verify retains
bounded integrity remediation, then supplies one full brief per affected finding
at cap exhaustion. Driver provenance and cause evidence remain explicit; moving
the brief does not move writer authority or grant additional retries.

Only work whose acceptance genuinely requires a decision depends on it:
`bd dep add <affected-work> <decision>`. Parentage is provenance, not a
prerequisite. A decision may block several work beads; a work bead may need
several decisions. Do not indiscriminately block the entire source/work epic or
create one implementation task per question.

Independently actionable work is split into schedulable beads under the existing
work epic, with dependencies on only its actual prerequisites. Split work enters
the existing driver-owned acceptance producer/resolution/persistence boundary
before dispatch: ordinary work has explicit criterion assignments, remediation
retains finding-goal attribution. Prose and labels do not manufacture bindings.
Use sibling work when nesting beneath a waiting source would inherit blockers;
actual Beads readiness, not a model-only graph, must demonstrate independence.

The source remains a final acceptance/join task only when split work is required
for its own goal; it then depends on that work and closes through ordinary
worker completion after the work finishes. Otherwise it may complete while
reported decisions block other work. Resolving a decision records human intent,
not implementation completion and not automatic source closure.

**Attributed scheduling state.** The driver parks eligible affected work waiting
on admitted decision prerequisites, and a source admitted with `Waiting`, as
`status=blocked`. It records typed durable wait attribution in Beads metadata,
including the work identity, phase/work-root context, and admitted prerequisite
references. Preserve unrelated metadata. The original work does not acquire a
duplicate Options brief or human-queue label merely because it waits. The driver
must not take ownership of an independently blocked/deferred/closed item or
another running dispatch based only on a newly reported decision.

**Resumption.** On Inbox chat exit, Loop/Todo startup, and scheduler refresh,
the driver re-reads attributed work and its current dependency graph. It returns
only matching driver-waiting work to `open` when all current prerequisites have
resolved and no independent human, semantic, infra, or deferred hold remains.
Other active blockers, cancelled/replaced attribution, malformed/missing
prerequisites, or incompatible status changes prevent reopening. Human adoption
of an independent hold cancels or replaces the wait attribution; a blocked
status alone cannot distinguish ownership. The driver never reopens closed work,
closes tasks on answered decisions, erases human resolutions, or restores labels
from a generic interactive-session verdict.

Wait admission/release is durable and idempotent. Interrupted or repeated
processing reuses the same decisions, edges, drafts, and attributed wait without
consuming retry budget or overwriting independent changes. Recheck current state
before writes; a conflict or ambiguous ownership fails visibly rather than
forcing a transition. A process restart needs no ephemeral cache fact to decide
that work is eligible. Waiting Loop workspaces and committed/uncommitted work
are preserved under [Workspaces](workspaces.md); waiting Todo drafts, fixed
batch state, notes, and cursors follow
[Todo](todo.md#decision-wait-and-resumption).

`partial/dependency_wait.md` is shared by Loop and Todo. It teaches durable
prerequisite declaration, non-closure while genuinely waiting, visible
attributed parking, and resumption without repeated discovery runs. The unit
terminal's spelling and framing belong to Protocol.

### Completion admission

[Acceptance](#verdict-gate-1).

Molecule-clean admission rechecks current known workflow dispositions, including
after restart. A provisional, rejected or unreconciled worker closure is not
completed work merely because Beads progress counts it as closed. It remains
unresolved until valid recovery/completion admission or a separately authorized
work disposition resolves the execution requirement. Such a disposition is not
implementation coverage and cannot waive current Gate obligations.

This rule concerns known workflow work, not a retrospective receipt requirement
for every ordinary historical closed bead. Cleanup and publication remain
separate: an ineligible-GC clone alone does not prove unresolved molecule work,
and permission to dispose of a workspace does not prove completion or authorize
publication. Missing disposable cache state cannot erase known unfinished work.

Post-push ancestor closure requires fresh per-candidate admission, including
ancestors outside the selected molecule. Re-read the epic, its direct children
and their known workflow dispositions, and the epic's current independent holds
or cancellation before closing. Closed children and an earlier clean push do not
override human intent or unresolved workflow completion. Skip held, cancelled or
ambiguously owned candidates with diagnostics, preserving their state; retain
inside-out closure for eligible ancestors without automatically restoring
status, labels or work. This grants no new decision-closing or inspection
mutation permission.

## Success Criteria

### Task acceptance

- The task-acceptance reference type and its metadata parser admit exactly one
  nonempty criterion-reference or remediation-reference variant. Mixed, empty,
  malformed, and unknown variants fail ingestion; parsed candidates cannot stand
  in for the separately resolved immutable dispatch value.
  [test?](task_acceptance_refs_are_exclusive_nonempty_variants)

<!-- prettier-ignore -->
- Actual dispatch distinguishes explicitly bound implementation tasks from
  driver-resolved finding remediation, including non-Todo and split tasks.
  Missing, malformed, or forged acceptance blocks before spawn without a
  purpose-changing fallback; a valid repair goal can address malformed
  acceptance while ordinary broken references still fail. [system?](nix run .#test-quint -- remediation-acceptance)

<!-- prettier-ignore -->
- The production worker-dispatch path rejects malformed, unknown, missing, or
  ambiguous declared acceptance references before spawning an agent rather than
  dropping them. Valid pending criteria remain assignable before their verifiers
  exist. Changing the dispatch snapshot requires fresh resolution and uses
  current bindings, not historical annotations or cached passes. [system?](nix run .#test-quint -- task-acceptance-dispatch)

### Workflow commands

- `loom loop --parallel N` (alias `-p N`) accepts a positive integer; non-
  positive or non-integer values fail with a clear error [test](default_is_one)

- `loom loop`'s worker queue resolution skips any bead with
  `issue_type == "epic"`, emitting an info-level log line naming the skipped
  epic. Sequential and parallel codepaths share the chokepoint
  [test](worker_queue_skips_epic_type_beads_with_info_log)

- Every successful `loom loop` invocation returns
  `LoopOutcome { gate: GateOutcome, .. }`; the binary's exit code is a pure
  function of the `GateOutcome` variant (`Success` → 0, `Fail` → non-zero,
  `NoGate` → 0) [test](loom_loop_exit_code_is_function_of_gate_outcome_variant)

- Every `loom loop` returning `LoopOutcome { gate: Success(r), .. }` references
  non-empty gate JSONL logs in `r.gate_log_paths`; each log contains a
  `gate_run_start` and matching successful `gate_run_end`, and the review
  evidence contains a terminal `AgentEvent` whose effective marker is complete.
  Holds for all execution modes (explicit bead roots, explicit epic roots,
  `--parallel`, and default active-epic continuous mode)
  [test](every_successful_loom_loop_references_completed_gate_logs)

- `run_parallel_loop` returns `Result<LoopOutcome, LoopError>` — identical type
  to the sequential codepath; parallel mode invokes the same molecule push-gate
  chokepoint after the batch drains, constructs `GateOutcome` from typed gate
  evidence, and returns. There is no parallel-specific summary type
  [test](parallel_codepath_returns_loop_outcome_with_gate_field)

- Outcome counts distinguish admitted worker outcomes from distinct admitted
  unresolved decisions. Repeated references count once and closed historical
  refs do not count; a completed source reporting five decisions yields one
  processed and five clarified rather than an exclusive clarified-source result.
  [test?](loop_outcome_counts_decisions_independently_of_worker_terminals)

- `loom loop` reads profile from bead label and spawns correct container
  [test](resolve_profile_reads_label)

- `loom loop` retries failed beads with previous error context
  [test](default_policy_is_two_retries)

- Before molecule push verification, the driver reconciles `.loom/integration`
  to the canonical `wrix.prekHooks` `core.hooksPath`, repairing stale store
  paths and failing loudly if the expected path cannot be resolved
  [test](push_gate_repairs_stale_integration_hooks_path)

- On molecule completion, after stabilization has drained promoted remediation,
  `loom loop` fetches/rebases against `origin/<integration-branch>`, resolves
  the remote tip and `HEAD` to the concrete OID pair for the actual push range,
  runs the actual prek pre-push chain for that range, then runs
  `loom gate review --diff <actual-push-range>` only after deterministic success
  [test](molecule_push_gate_verifies_and_reviews_actual_push_range)

- After each per-bead agent run signals Success and the bead's branch is rebased
  onto the integration branch + ff'd at the loom workspace (inside
  `index.lock`), the loop invokes exactly
  `loom gate verify --diff <pre-integration-head>..HEAD`. The per-bead hot path
  never invokes focused LLM review or `mint`
  [test](exec_per_bead_gate_invokes_post_integration_verify_only)

- The molecule-completion handoff evidence is populated from typed `GateRun`,
  `VerifiedScope`, and `ReviewedScope` values parsed from actual gate JSONL
  logs. No trust field is left at default `None` when a child process produced a
  parseable run; absence surfaces as a `GateFail` variant per
  [Loop Outcome Types](#loop-outcome-types)
  [test](handoff_evidence_populates_typed_gate_scope_values)

- When the molecule-completion audit review produces ≥1 unsuppressed streamed
  `LOOM_FINDING:` record and a `LOOM_CONCERN:` terminator, `route="deferred"`
  findings merge into the molecule's deferred remediation set and cause another
  stabilization pass within the molecule iteration cap; `route="clarify"`
  findings materialize one `loom:clarify` bead per finding hash. If every
  streamed finding is suppressed, the effective review marker is Complete and no
  recovery prompt is produced. Mint does NOT fire during the per-bead hot path;
  deferred findings are promoted by `loom gate mint -m <molecule-id>` during
  stabilization
  [test](molecule_completion_review_routes_findings_to_stabilization_or_clarify)

- A molecule-completion review finding with `route="blocking"` refuses the push
  and creates or reuses same-molecule remediation work; already-integrated
  original beads are not reopened solely because the push-stage review found a
  concern
  [test](molecule_review_blocking_finding_creates_same_molecule_remediation)

- A molecule-completion review finding with `route="deferred"` merges into a
  molecule child bead with `status=deferred` and label `loom:deferred`;
  `bd ready` does not return it until molecule stabilization promotes it
  [test](molecule_review_deferred_finding_creates_deferred_bead)

- Structural bd conflicts while recording deferred or clarify findings route the
  molecule to `loom:blocked` with cause `gate-routing-structural-violation`;
  already-integrated commits are not unwound
  [test](molecule_routes_gate_routing_structural_conflict_to_blocked)

- A synthetic post-integrate verify failure writes a durable gate log under
  `.loom/logs/gate/` containing command argv, resolved scope, per-lane
  hook/verifier results, exit code, stdout/stderr tails, integration SHA, bead
  id, retry attempt, rollback state, and log path
  [test](post_integrate_verify_failure_writes_durable_gate_log)

- The `driver_event` emitted for `post-integrate-fail` names the gate log path
  in its payload / rendered summary, and retry attempts produce distinct log
  paths while successful integration flow is unchanged
  [test](gate_invocations_write_separate_jsonl_logs_with_parent_breadcrumb)

- Transient errors while recording deferred or clarify findings thread their
  detail into `PreviousFailure` and re-run through the existing per-bead
  recovery loop bounded by `[loop] max_retries`; after exhaustion the bead
  routes to `loom:blocked` with cause `retry-exhausted`
  [test](loop_per_bead_routes_gate_recording_errors_through_recovery_loop_bounded_by_max_retries)

- `loom loop`'s outer loop, after original non-deferred work drains, invokes
  `loom gate mint -m <molecule-id>` to promote deferred remediation beads,
  re-polls `bd ready`, and processes promoted remediation before the final push
  gate can succeed. The outer loop is bounded by `[loop] max_iterations`
  (default 10) and exits cleanly on push success, a fully-stuck molecule, or
  counter exhaustion
  [test](continuous_outer_loop_promotes_deferred_remediation_then_exits_on_stall)

- Push gate supplies the completed molecule state and actual push-range gate
  runs to the gate-owned `GateSuccess` constructor. Receipt rejection refuses
  the push and becomes `GateOutcome::Fail`; the accepted evidence and matching
  rules are defined only in
  [Gate — Gate success receipt](gate.md#gate-success-receipt)
  [test](push_gate_evaluates_typed_evidence_and_marker_coverage)

- On a **clean** push gate the `MarkerProof` is minted to `.loom/marker.json`
  **immediately before** `git push`, inside the gate's critical section, after
  deterministic pre-push and review have both covered the actual push range. A
  **refused** push (unresolved blocked/clarify/deferred/infra bead, pre-push
  failure, verify-fail, review-concern, integrity finding, or missing marker
  coverage) mints nothing. A missing or invalid marker falls the pre-push
  consumer through to running hooks rather than failing the push by itself
  [test](clean_push_mints_marker_after_covered_verify_and_review)

- Push gate refuses when `loom gate review`'s `--diff`-scoped invocation emits
  `LOOM_CONCERN`; molecule routes to recovery with cause `review-concern`
  [test](push_blocked_on_review_concern_with_id_payload)

- Push gate handles the integrity-gate findings that
  [Verify — Integrity gate](verify.md#integrity-gate) defines as
  push-gate-terminal within the molecule's diff scope by **recovery-first then
  escalate**: while the molecule's iteration counter is below cap, the gate
  normalizes findings to typed `Finding`s and merges them into the molecule's
  deferred remediation set (per
  [Findings — Findings and Minting](findings.md#findings-and-minting-1)).
  Findings coalesce by lead spec / concern family, the push is refused, the
  counter is incremented, `loom gate mint -m` promotes deferred remediation, and
  the outer loop re-enters so the worker can address the batch. On cap
  exhaustion, the gate admits the complete dedicated-decision set under the work
  epic, preserving full per-finding alternatives and partial resolution per
  [Verify — Integrity gate](verify.md#integrity-gate). The epic gets no copied
  Options brief or blanket decision dependency; publication remains refused
  while required findings or human decisions remain unresolved.
  [test?](push_gate_recovers_integrity_findings_then_admits_decision_batch_at_cap)

- Push gate refuses on any verify-tier dispatch error (exit code 2 = unknown
  verifier, command not found); dispatch errors count as fails, not skips
  [test](push_blocked_on_verify_dispatch_error)

- `loom loop` auto-iterates on remediation beads (up to max iterations)
  [test](default_cap_matches_spec)

- `loom loop --help` documents `[BEAD_OR_EPIC_ID ...]`, the default
  `loom:active` work epic, interspersed options, explicit ambient-host meaning
  of `--host-key`, and absence of `--spec` / `--once` / `--all-specs`
  [test](loom_loop_help_documents_work_roots_and_removed_selectors)

### Verdict gate

<!-- prettier-ignore -->
- Worker feedback, trusted post-integration verification, and full publication
  remain distinct stages. Missing worker capabilities remain visible feedback
  gaps to be discharged by the required trusted stage. [system?](nix run .#test-quint -- stage-handoff)

<!-- prettier-ignore -->
- The driver carries Gate-admitted publication-attempt context through actual
  verification, review, and Git pre-push consumption. Failed or interrupted
  pushes, including process loss before outcome recording, cannot resume that
  context on retry. The production retry path observes actual remote state and
  obtains new admission under Gate's evidence rules, not authority from an old
  receipt or marker. [system?](nix run .#test-quint -- workflow-publication-attempt)

- After every per-bead `loom loop` worker phase, the verdict-gate decision table
  classifies the terminal marker plus mechanical signals (bd-closed, diff, tree
  cleanliness, current prerequisites and holds/cancellation) without an LLM call.
  `Blocked` routes on its checked nonblank reason, not a semantic options
  judgment. First-match precedence handles overlapping failures, and a non-empty
  diff cannot pass as `Noop`.
  [test?](loop_verdict_table_is_total_and_precedence_is_stable)

<!-- prettier-ignore -->
- `phase_verdict::decide()` is invoked from `loom loop`'s per-bead exit AND from
  `loom gate review`'s phase-end; no production site inlines ad-hoc marker →
  outcome classification (FR12) [check](cargo run -p loom-walk -- phase_verdict_decide_called_from_production)

- `loom loop` never invokes `bd close` on a bead it dispatched; closure is the
  agent's responsibility and the `bd-closed` column is observed post-hoc.
  Dependency release and decision reporting do not close the source on the
  agent's behalf; verify closure ownership across all admitted terminal outcomes.
  [test?](loop_preserves_worker_closure_ownership_with_decision_records)

- `LOOM_BLOCKED` agent marker with a non-empty reason transitions the bead to
  `[blocked]` and skips the recovery loop
  [test](blocked_marker_routes_to_blocked_with_reason)

- Nonterminal decision reporting admits dedicated decision beads, not a clarify
  status on the emitting task. Completion of that task remains possible when
  its own acceptance has no unresolved prerequisite or independent hold/cancellation.
  [test?](clarify_record_and_completion_have_independent_source_outcomes)

- Every direct decision reference resolves against the current task/work-root
  and graph before effects; a unique canonical Options brief is required for the
  clarification queue. Missing or malformed briefs retain the per-decision
  `clarify-without-options` fallback; invalid references cannot
  silently disappear or authorize source completion.
  [test?](decision_batch_admission_resolves_every_reference_and_brief)

- New decision candidates remain blocked and outside human queues and worker
  dispatch until contextual admission; interrupted or unreported candidates
  survive for repair/reuse without fabricated acceptance bindings.
  [test?](decision_candidates_stay_staged_until_contextual_admission)

- Decision handoffs account for every persisted candidate attributable to the
  producer/work-root through reported IDs or valid prior dispositions before new
  queue/wait effects. An omitted staged candidate rejects `Complete`/`Noop`
  before integration, including after agent-side closure, and cannot hide behind
  `Waiting`; unrelated producers are not swept in, ambiguous ownership fails
  visibly, and prior/closed decisions need no redundant report.
  [test?](loop_handoff_rejects_unaccounted_owned_decision_candidates)

- Reference/graph errors reject the full new handoff before queue/wait effects,
  retaining valid siblings and existing human state. Brief-only failures downgrade
  the defective decision while valid siblings enter the clarification queue,
  without coupling source completion to unrelated decisions.
  [test?](decision_batch_failure_dispositions_preserve_context_and_independent_outcomes)

- Previously admitted closed decision references replay without queue creation,
  reopening, new brief requirements, or loss of the recorded human outcome.
  [test?](closed_decision_reference_replay_preserves_resolution_and_queue_identity)

- Brief downgrades emit `DriverKind::ClarifyDowngraded`, write the
  `clarify-without-options` breadcrumb on the defective decision, and pair its
  label/status mutation with `DriverKind::BdStateTransition`; they do not mutate
  the source into a duplicate human-queue item.
  [test?](decision_brief_downgrade_emits_events_on_defective_decision_only)

- The decoded `Retry.reason` produces `agent-retry` recovery and
  `PreviousFailure::AgentRetry { reason }`, consuming one existing retry slot;
  unrelated preceding prose cannot supply or replace the reason.
  [test?](agent_retry_json_reason_consumes_budget_without_prose_scraping)

- `LOOM_RETRY` recovery exhaustion → `loom:blocked` with cause `retry-exhausted`
  (the same exhaustion path as other driver-detected recoveries)
  [test](consecutive_agent_retry_exhaustion_routes_to_loom_blocked_retry_exhausted)

- No marker emitted → recovery with cause `swallowed-marker`
  [test](missing_marker_routes_to_swallowed_marker_recovery)

- Admitted dependency waiting visibly parks non-closed work with typed durable
  attribution and an active prerequisite, preserving its workspace/branch
  without integration, per-bead gate, retry consumption, or semantic/clarify/infra
  queue labels.
  [test?](attributed_dependency_waits_park_without_integration_or_retry)

- `LOOM_WAITING` without an active declared blocker is invalid, routes through
  visible recovery as `invalid-waiting`, and never silently parks the bead
  [test](waiting_marker_without_blocker_is_recovery_not_silent_parking)

- Parallel loop handling preserves the same waiting semantics: the waiting
  slot's workspace remains, its branch is not merged, and sibling ready work
  continues [test?](parallel_attributed_wait_preserves_workspace_and_ready_siblings)

- Completion cannot bypass an active prerequisite for the source's own
  acceptance even if the agent closed the bead; reported decisions affecting
  only other work do not prevent independent source completion.
  [test?](completion_rejects_active_source_prerequisites_without_coupling_other_decisions)

- Current independent holds or authorized cancellation veto `Complete`/`Noop`
  before integration or intentional no-work acceptance, including human changes
  during the worker session and after agent-side closure. Preserve human state,
  show the conflict, and do not automatically reopen or redispatch the source.
  [test?](completion_rechecks_human_holds_and_cancellation_before_acceptance)

- `LOOM_COMPLETE` + resolved own prerequisites + no independent hold/cancellation + bead not bd-closed → recovery
  with cause `incomplete-signaling`
  [test](complete_without_bd_closed_routes_to_incomplete_signaling)

- `LOOM_COMPLETE` + resolved own prerequisites + no independent hold/cancellation + closed + empty diff → recovery
  with cause `zero-progress`
  [test](complete_with_empty_diff_routes_to_zero_progress)

- `LOOM_NOOP` + resolved own prerequisites + no independent hold/cancellation + closed + clean tree + empty diff →
  accepted as intentional no-work output
  rather than zero-progress; no post-integration verify runs for an empty bead
  diff [test](run_bead_noop_empty_branch_is_done_not_zero_progress)

- `LOOM_COMPLETE` + resolved own prerequisites + no independent hold/cancellation + closed + non-empty diff + dirty working tree
  (`git status --porcelain` non-empty) → recovery with cause `tree-not-clean`;
  post-integration verify is NOT run (recovery precedes it so verifiers don't
  execute against a half-staged tree); `previous_failure` lists the dirty paths
  capped at 30
  [test](complete_with_dirty_tree_routes_to_tree_not_clean_before_verify)

- `LOOM_NOOP` + resolved own prerequisites + no independent hold/cancellation + closed + dirty working tree → recovery with cause
  `tree-not-clean` (NOOP claims "no work needed" but the tree disagrees;
  surfacing the discrepancy is more useful than letting the bead close on a
  false negative) [test](noop_with_dirty_tree_routes_to_tree_not_clean)

- `tree-not-clean` detail enumerates the dirty paths (modified,
  staged-but-uncommitted, and untracked outside the gitignore set) capped at 30
  entries with a "+N more" suffix when truncated
  [test](tree_not_clean_detail_enumerates_and_caps_dirty_paths)

- Post-integration per-bead verify runs the project pre-commit lane and every
  affected deterministic annotation required for integration by
  [Gate's verify contract](gate.md#deterministic-verify-lanes), including
  stage-required system checks, for `<pre-integration-head>..HEAD`; no eligible
  lane short-circuits another, and per-hook/verifier outcomes plus stderr are
  captured
  [test?](post_integration_verify_includes_stage_required_system_annotations)

- One or more `loom gate verify` failures → recovery with cause `verify-fail`;
  `previous_failure` carries every failure (not just the first), with a
  4000-char budget split across them
  [test](verify_fail_carries_every_failure_block_for_previous_failure)

- Per-bead focused review does not run after post-integration verify; mechanical
  verify failure routes directly to `verify-fail` / `post-integrate-fail` with
  gate-log evidence, while molecule- completion review runs only after
  deterministic pre-push success
  [test](post_integrate_verify_failure_writes_durable_gate_log)

- `LOOM_CONCERN` → recovery with cause `review-concern`; the detail names which
  concern triggered (live-path / mock / scope / judge / style-rule)
  [test](concern_marker_with_streamed_findings_routes_to_review_concern_recovery)

- Review-phase verdict classification populates `GateInputs.streamed_findings`
  from the parsed walk output. A well-formed `LOOM_CONCERN` with one or more
  streamed `LOOM_FINDING:` records routes to
  `RecoveryCause::ReviewConcern { summary, findings }`; worker phases, which
  have no review findings stream, reject review-only markers before verdict
  classification
  [test](classify_review_phase_invokes_parse_walk_output_and_threads_findings_through_gate_inputs)

- Concern handling has one production route through typed findings and
  `RecoveryCause::ReviewConcern`; no alternate review-error route can bypass
  that classification
  [test](no_path_constructs_concern_without_bead_deltas_in_production_harness_lane)

- Recovery iter < `[loop] max_iterations` (default 10) → promotes deferred
  remediation OR retries the bead with prior failure context
  [test](under_max_recovers_with_previous_failure)

- Every remediation bead created by the verdict gate is bonded to the
  originating bead's molecule via `bd mol bond` before becoming eligible for
  `loom loop` dispatch; the bond is atomic with bead creation (no transient
  orphan window) [test](spawned_outcome_bonds_to_origins_parent_molecule)

- If the originating bead is unbonded (no molecule), the verdict gate refuses to
  create remediation state and instead applies `loom:blocked` with cause
  `unbonded-origin` to surface the upstream inconsistency
  [test](refused_outcome_applies_unbonded_origin_blocked_to_origin)

- The push gate walks `bd mol progress <id>` and refuses to push when any
  non-closed bead in the molecule — including bonded remediation and decision
  beads — carries `loom:blocked`, `loom:clarify`, `loom:deferred`, or
  `loom:infra`. Closed historical labels alone do not block publication; actual
  unresolved work, prerequisites and required gate evidence still do. An orphan
  remediation bead would slip past this check, so the bond invariant remains
  necessary
  [test](remediation_beads_under_cap_auto_iterate)

- Non-integrity recovery at the molecule iteration cap applies `loom:blocked`
  with its cause in `bd update --notes`. Integrity cap exhaustion instead follows
  [Verify's dedicated-decision escalation](verify.md#integrity-gate); this rule
  does not replace that path or merge transport/in-session retry budgets.
  [test](at_or_above_max_applies_blocked_with_retry_exhausted_cause)

- Iteration count is **work-epic-level** state (cached in
  `work_epics.iteration_count`, not on individual beads) and survives
  `retry → [running]` round-trips; every promoted remediation pass consumes one
  slot of `[loop] max_iterations`
  [test](iteration_counter_round_trips_through_cache_db)

- Agent event-stream failures are classified by `first_event_seen`: EOF before
  the first canonical `source = "agent"` event is retryable `infra-preflight`;
  EOF after one or more agent-sourced events but before `session_complete` is
  retryable `infra-interrupted`; an explicit worker `LOOM_BLOCKED` remains
  semantic `loom:blocked`
  [test](agent_stream_failure_classifier_distinguishes_preflight_interrupted_and_blocked)

- Agent-output framing/JSON/phase errors after an otherwise completed session
  use worker protocol recovery, not the transport/infra budget. Backend
  RPC-frame errors and premature streams retain their infra classification.
  [test?](completed_session_output_errors_use_worker_not_transport_recovery)

- Retryable infra failures use a per-bead, per-`loom loop` budget from
  `[loop.infra] max_attempts` (default 3), move failed beads to the tail of an
  in-memory retry queue, continue other ready work, and retry without wall-clock
  cooldown/backoff while attempts remain
  [test](infra_failures_round_robin_per_bead_without_cooldown)

- EOF before the first agent event retries under the infra budget; after
  exhaustion the bead is paused as `status=blocked` + `loom:infra` and never
  labelled `loom:blocked`
  [test](preflight_eof_retries_then_surfaces_infra_not_semantic_blocked)

- Partial event stream followed by EOF routes to `infra-interrupted`, includes
  `first_event_seen=true`, and follows the same infra retry budget instead of
  semantic worker recovery
  [test](partial_stream_eof_classifies_interrupted_infra)

- Driver infra-failure events include phase, first-event-seen, attempt/max
  attempts, infra class/cause, agent/container exit status when known, and
  stderr tail or spawn error when available
  [test](infra_failure_driver_event_payload_carries_stream_diagnostics)

- Static dispatch diagnostics such as undeclared `profile:X`, missing runtime
  for a declared profile, invalid spawn config, missing agent binary, or
  `workspace-recovery-failed` skip transport retry and surface immediately as
  `status=blocked` + `loom:infra` with notes naming the requested value,
  declared/available set, or preserved workspace-recovery failure detail.
  Missing or malformed `LOOM_PROFILES_MANIFEST` remains a startup/global error
  before bead selection.
  [test](static_dispatch_failures_surface_as_infra_without_retry)

- A prior attempt that reached `session_complete` is not overwritten by a later
  retry's pre-stream EOF; the later failure records infra diagnostics without
  converting the bead to semantic `loom:blocked`
  [test](prior_session_complete_not_overwritten_by_later_preflight_eof)

- Infra retry budget is driver-memory only; a fresh `loom loop` invocation gets
  a fresh per-bead budget and proactively retries selected work-root beads
  labelled `loom:infra`, clearing stale infra state when redispatching
  [test](fresh_loop_retries_loom_infra_beads_with_fresh_budget)

- The push gate refuses to push while any non-closed bead in the molecule carries
  `loom:blocked`, `loom:clarify`, or `loom:infra`
  [test](clarify_or_infra_present_stops_without_pushing)

- Closed decisions may retain queue labels and briefs as history without alone
  preventing a clean publication or post-push epic closure. Live decisions,
  unresolved prerequisites, holds and current gate failures still refuse
  publication; clearing a historical label is not required or safety evidence.
  [test?](publication_ignores_closed_queue_history_without_ignoring_live_blockers)

- Observer-driven abort (`EventSink::react()` returning `SessionCommand::Abort`)
  classifies as recovery cause `observer-abort` with detail naming the
  responsible observer + the reason it gave; distinct from `swallowed-marker`
  (which means the agent ended without a marker on its own, not under driver
  cancel)
  [test](observer_abort_routes_to_observer_abort_distinct_from_swallowed_marker)

- After initial Git and Beads publication succeed, the driver closes only
  freshly admitted ancestor epics with closed direct children and no unresolved
  known workflow completion, independent hold or cancellation. Each admitted
  close emits one `DriverKind::EpicAutoClosed` with the epic id; ambiguous
  admission preserves state with diagnostics.
  [test?](epic_auto_close_requires_current_ancestor_admission)

- Ancestor auto-close preserves independent human holds/cancellation, including
  late changes and ancestors outside the selected molecule. Closed children or
  an earlier clean push cannot overwrite their status, labels or disposition.
  [test?](epic_auto_close_preserves_independent_holds_and_cancellation)

- Molecule-clean admission rejects known provisional, rejected or unreconciled
  workflow closures after restart until valid completion/recovery or an
  authorized work disposition. Historical closed work needs no retrospective
  receipt; GC ineligibility or workspace disposal alone is not publication
  authority or a publication blocker.
  [test?](molecule_clean_rejects_known_unaccepted_workflow_closures)

- The trusted publication controller constructs its Beads synchronization
  command as `wrix beads push`, never the removed `beads-push` helper or
  `bd dolt push`. Standalone gate review remains inspection-only under
  [Gate's command contract](gate.md#commands).
  [test](beads_push_argv_invokes_wrix_beads_push_not_bd_dolt_push)

- Loop-owned molecule handoff publishes Beads through the current
  `wrix beads push` CLI, never the removed `beads-push` helper or `bd dolt push`
  [test](molecule_handoff_publishes_beads_through_wrix_cli)

- Epic auto-close does not fire while any direct child of the candidate epic
  carries `status != "closed"` (`open`, `in_progress`, or `deferred`).
  [test](epic_does_not_auto_close_when_any_child_non_closed)

- Epic auto-close does not fire on any non-Clean push-gate verdict
  (`LOOM_CONCERN`, any non-closed bead carrying `loom:blocked` or `loom:clarify`); only the
  `Clean` arm reaches the walk.
  [test](epic_does_not_auto_close_on_non_clean_review_verdict)

- Eligible nested epics close inside-out in one publication-controller pass:
  closing an inner epic re-enqueues its parent for fresh admission, so an
  eligible grandparent retires in the same `Clean` walk.
  [test](nested_epics_close_inside_out_in_one_pass)

- Epic auto-close runs only after the initial `git push` and `wrix beads push`
  both succeed; either initial publication failure skips the close walk. Later
  close writes are not covered by that earlier Beads synchronization.
  [test?](epic_auto_close_runs_only_after_initial_git_and_beads_publication)

### Decision scheduling

- Driver wait release reopens only matching attributed work after all current
  prerequisites resolve; independent holds, cancelled attribution, malformed
  state, other blockers, and closed work are preserved.
  [test?](decision_wait_resumption_preserves_human_holds_and_closed_work)

- Repeated or interrupted admission/resumption reuses durable decision and wait
  state without duplicate queue items, lost drafts, overwritten unrelated
  metadata, or retry-budget consumption; ambiguous ownership fails visibly.
  [test?](decision_wait_admission_and_resumption_are_restart_safe)

<!-- prettier-ignore -->
- Actual Beads dependency/ready selection and production Loop/Todo/Inbox paths
  demonstrate partial decision resolution: three of five answers unlock only
  their affected bound work, remaining decisions block only genuine
  prerequisites, and an independent reporting source can complete. Split-goal
  join tasks wait for implementation, not merely answers; sibling work avoids
  inherited ancestor blockers, and nested decision items stay visible in Inbox
  after their discovering source closes. Staged/mixed/closed-reference replay and
  driver conflict/integrity escalation preserve queue identity, retry caps and
  independent follow-up readiness through actual Beads behavior. Omitted owned
  candidates prevent positive completion; independent source closure preserves
  live decision children and never bypasses genuine own prerequisites or holds.
  Actual pinned Beads closure guards are exercised, not inferred from help or
  defeated with blanket force/reparenting/child closure. Late human holds or
  cancellation veto stale positive completion without state reversion; closed
  historical queue labels do not conceal live blockers or obstruct an otherwise
  clean publication. Actual restart also exercises
  [Workspaces' cleanup admission](workspaces.md#cleanup-admission), preserving
  unaccepted/held closed sources rather than treating closure as safe disposal.
  Known unaccepted workflow closure cannot become molecule-clean publication
  authority on restart, while ordinary historical closures remain usable.
  Ancestor closure rechecks current human holds/cancellation, including outside
  the molecule and after publication, without treating GC ineligibility alone as
  unfinished work or disposal permission as passing Gate evidence.
  [system?](nix run .#test-decision-waits)

### Auxiliary commands

- `loom status` prints the active work epic, any pending `loom:todo` work epic,
  cached iteration counts, and cache health; no active spec/current-spec value
  is displayed or read [test](status_reports_active_work_epic_not_current_spec)
  The `loom logs` inspection surface is owned by [Events](events.md).

### Cache database

- Review that depends on a work-epic range refuses missing cache state rather
  than interpreting it as clean work. Explicit diff/tree review scopes remain
  independent of work-epic range context.
  [test](integrity_findings_rejects_work_epic_without_range)

- Review refuses a missing selected work-epic counter cache row rather than
  interpreting it as a zero iteration count.
  [test](iteration_count_rejects_missing_work_epic_cache)

### Typed `PreviousFailure`

- `PreviousFailure` is a tagged enum with variants `DriverNotice`,
  `VerifyFailures(Vec<VerifierFailure>)`, `ReviewConcern { summary, findings }`,
  `BadWalk(BadWalk)`, `BuildFailure`,
  `TreeNotClean { dirty_paths: Vec<String> }`,
  `PostIntegrateFail { failures, gate_log_path }`,
  `IntegrationConflict { files, new_base_sha }`, and
  `AgentRetry { reason: String }` — not a free string
  [test](previous_failure_public_variant_contract_is_constructible)

- `PreviousFailure::AgentRetry { reason }` carries the decoded JSON reason and
  renders it with cause-appropriate bounded recovery guidance rather than
  reconstructing a reason from adjacent stdout.
  [test?](agent_retry_json_reason_reaches_previous_failure_rendering)

<!-- prettier-ignore -->
- `TreeNotClean` variant carries `dirty_paths: Vec<String>` capped at 30 entries
  by the driver before construction [check](grep -q 'TreeNotClean' crates/loom-templates/src/previous_failure.rs)

<!-- prettier-ignore -->
- `PostIntegrateFail` variant carries `failures: Vec<VerifierFailure>` and
  `gate_log_path: PathBuf` directly; populated when the loom-workspace per-bead
  integration step's verify against the integrated tree fails after the bead's
  own verify passed at its bead workspace. Review concerns are not a possible
  cause — they route through `ReviewConcern` / `BadWalk` after verify succeeds.
  [check](grep -q 'gate_log_path' crates/loom-templates/src/previous_failure.rs)

<!-- prettier-ignore -->
- `IntegrationConflict` variant carries the conflict file list and new
  integration base SHA for the single agent retry after a driver-side rebase
  conflict [check](grep -q 'IntegrationConflict' crates/loom-templates/src/previous_failure.rs)

- `DriverNoticeCause` enum covers `SwallowedMarker`, `IncompleteSignaling`,
  `ZeroProgress`, `ObserverAbort`, `RetryExhausted`, `UnbondedOrigin`
  [test](driver_notice_cause_labels_match_spec_strings)

### Attempt counter

- `LoopContext` carries `attempt: u32`; field is `0` on fresh bead dispatch
  [test](attempt_zero_on_fresh_bead_dispatch)

- Attempt counter is per-bead in-session: fix-up beads start at `attempt = 0`
  regardless of the failing bead's prior attempts
  [test](fix_up_bead_starts_at_attempt_zero)

- Attempt counter is bounded by `[loop] max_retries` (default 2)
  [test](failed_bead_retries_with_previous_failure_then_blocks)

### Review recovery

- Push-gate integrity findings recover via deferred remediation until the
  molecule's iteration counter exhausts: the verdict gate normalizes
  `UnresolvedAnnotation`, `StubTestFunction`, and `UnneededPendingMarker` into
  typed `Finding`s, merges them into the molecule's deferred remediation set,
  promotes them with `loom gate mint -m/--molecule`, refuses the push,
  increments the counter, and re-enters the loop. On cap exhaustion, the gate
  admits dedicated decision children with full per-finding Options briefs and
  scoped dependencies under [Verify's cap escalation](verify.md#integrity-gate).
  It does not copy Options onto the work epic or prevent independent follow-up
  work by installing blanket epic blockers.
  [test?](push_gate_recovers_integrity_findings_then_admits_decision_batch_at_cap)

- A walk that terminates with `LOOM_RETRY` (review itself could not run for
  environmental reasons) routes to recovery cause `agent-retry` per
  [Loop — Verdict Gate](#verdict-gate); consumes one `[loop] max_retries` slot;
  exhaustion escalates to `loom:blocked` with cause `retry-exhausted`
  [test](retry_marker_routes_to_agent_retry_recovery_cause)

### Worker self-check

- The bead-container worker runs the injected exact self-check range
  (`loom gate verify --diff <bead-base>..HEAD`, or `@{u}..HEAD` only when
  upstream is that base) before emitting `LOOM_COMPLETE`, reruns it after any
  later commit or hook-generated change, and performs prompt-level self-review
  before final marker emit
  [judge](../tests/judges/loom.sh#judge_loop_preflight_exact_range_and_self_review)

## Requirements

### Command entrypoint

[Acceptance](#workflow-commands).

- `loom loop [OPTIONS] [BEAD_OR_EPIC_ID ...]` — execute work. With no ids, runs
  the sole `loom:active` work epic. With ids, each positional may be a task bead
  (run exactly that bead) or an epic (run ready child work under that
  epic/molecule). Options may appear before, between, or after ids. Repository
  keys are mandatory at startup unless `--host-key` explicitly opts into ambient
  host Git policy. The loop pulls ready child beads filtered to exclude semantic
  `loom:blocked` / `loom:clarify` beads; `loom:infra` diagnostic beads are a
  driver-owned retry queue and are retried under the infra policy before a work
  root is considered fully stuck. An epic positional is a work root, never a
  worker task itself. Under `--parallel N`, a clarify or block on one of the N
  concurrent beads does not cancel the others. On work-epic completion, the
  driver fetches/rebases against `origin/<integration-branch>`, verifies the
  actual push range via the prek pre-push chain, runs
  `loom gate review --diff <actual-push-range>`, then evaluates the push gate
  under [the publication contract](gate.md#gate-success-receipt). The outer loop
  iterates over work-epic passes (initial pass + each promoted remediation pass)
  bounded by `[loop] max_iterations`. **`loom loop` returns a typed
  [`LoopOutcome`](#loop-outcome-types) whose `gate: GateOutcome` field is
  non-optional; the binary's exit code is a pure function of the `GateOutcome`
  variant.**

##### Worker and per-bead integration checks

[Acceptance](#verdict-gate-1).

**Agent self-check before marker emit.** In `loom loop`'s bead container, the
worker runs the injected exact self-check range before emitting `LOOM_COMPLETE`:
`loom gate verify --diff <bead-base>..HEAD` where `<bead-base>` is the resolved
base commit the driver supplied for the bead workspace. `@{u}..HEAD` is
acceptable only when the upstream is the intended injected base; `--diff HEAD`
is a diagnostic working-tree check and is not the worker completion contract. If
the agent makes another commit or a hook changes the tree after the self-check,
the prompt requires the agent to rerun the self-check before final marker emit.

The self-check uses the shared
[deterministic verify lanes](gate.md#deterministic-verify-lanes): project
pre-commit hooks via prek, then the affected annotation obligations for that
stage. This is feedback for the agent, not authoritative gate evidence. The
worker also performs prompt-level self-review before completion: re-read the
bead criteria, inspect the diff, check style/spec fit, and fix issues or emit
the appropriate worker self-report marker.

**Driver per-bead integration.** After the bead branch rebases and fast-forwards
into `.loom/integration`, the driver runs deterministic verification only:

```bash
loom gate verify --diff <pre-integration-head>..HEAD
```

This post-integration verify runs in the loom workspace against the actual
integrated tree. It does not pass `--bead`, `--spec`, or a hidden tier override,
and it does not run a focused LLM review by default. Any failure rolls the
integration branch back to `<pre-integration-head>`, records the gate log path
in `previous_failure`, and retries or reopens the same bead through the existing
recovery policy. The worker's `bd close` is provisional until this deterministic
integration gate passes.

#### Recovery

[Acceptance](#review-recovery).

Per-stage flag handling:

- **Plan** — interview held until the spec is amended (claim surfaced, clash
  resolved, or explicitly out-of-scope'd). User authorisation required to ship a
  spec with unresolved gaps.
- **Worker / per-bead integration** — worker self-check failures are
  prompt-level feedback; driver post-integration deterministic failures roll
  back the integration and enter same-bead recovery with `previous_failure`
  rendered into the next prompt.
- **Push** — deterministic pre-push failures skip review, refuse the push, and
  create or reuse same-molecule remediation work. Review `route="blocking"`
  findings do the same. `route="deferred"` findings are stored on
  `status=deferred` / `loom:deferred` molecule remediation beads and are not
  returned by `bd ready` until stabilization promotes them. `route="clarify"`
  findings create or update one `loom:clarify` bead per finding hash;
  `loom inbox` resolves the clarify. Clashes never trigger fresh-agent retry of
  an already- integrated original bead.
- **Standing** — `loom gate mint --tree` walks the deterministic verifiers and
  the LLM rubric, materializes typed findings as ready remediation batches
  grouped by lead spec / concern family (plus single-finding clarify beads for
  any clarify-route findings) under one active standing remediation work epic
  while ensuring each owning spec has exactly one spec epic. If no actionable
  batch remains after suppression/dedup, it creates no work epic. Invariant
  clashes surface via `loom:clarify` on the minted single-finding clarify bead;
  resolved in the next `loom inbox` walk. See
  [_Findings and Minting_](findings.md#findings-and-minting-1) for the deferred
  remediation processing flow.

##### Post-hoc recovery — when the push gate was skipped

[Acceptance](#verdict-gate-1).

**Use case.** A molecule's beads closed without `GateSuccess` being constructed
— e.g., a legacy run from before the type-shape enforcement landed, or a manual
`bd close` outside the gate. The work shipped but was never audited; the
codebase has unverified divergence from the spec. The original push-stage range
no longer applies because HEAD and origin have moved on to subsequent work, and
reconstructing the old range would mix unrelated downstream commits.

**Canonical recovery path:** `loom gate mint --tree`. The standing- safety-net
scope is exactly what's needed — walk the full spec set against the full
implementation, no diff math, no dependence on a still-valid `loom.todo_cursor`,
with remediation beads grouped by lead spec under one active remediation work
epic as findings emerge.

```bash
loom gate mint --tree                  # walks every spec; mints one active
                                       #   remediation work epic when needed
loom loop                              # runs the active remediation epic
```

For inspection without minting, `loom gate audit --tree` runs the same walk and
prints findings to stdout without bd writes.

No explicit seeding step is required — mint ensures spec epics via the lifecycle
in [Todo — Spec and Work Epic Lifecycle](todo.md#spec-and-work-epic-lifecycle)
and creates a single active work epic for actionable remediation. Recovery is
just the standing safety net exercised explicitly.

**Compositional safety.** The recovery flow's `loom loop` produces `GateOutcome`
per molecule — silent skip is structurally unrepresentable (see
[Loop](#loop-outcome-types)). The worker-queue filter
([work-root selection](#command-entrypoint)) prevents the agent from receiving
an epic as a worker task. Together, the conditions for the original gate-skip
class are structurally unreachable.

### Scheduling and publication

1. **Bead dispatch** — `loom loop --parallel N` (alias `-p N`) dispatches up to
   N ready beads, each in its own clone of the loom workspace under
   `.loom/beads/<id>/` on a per-bead branch. The operator's `/workspace` is
   never the bead's workdir. `--parallel 1` (default) runs one bead at a time;
   `--parallel N > 1` runs N concurrently. Before each bead-worker dispatch,
   Loom preserves any dirty bead workspace work in an unapplied recovery stash,
   rebases committed bead work onto the current integration tip when possible,
   and exposes the stash/alignment result through loop-only `workspace_recovery`
   prompt context. After workers finish, the driver fetches each bead branch
   from its bead workspace path into the loom workspace, then rebases +
   fast-forwards into the integration branch sequentially (per
   [Verdict Gate § Loom-workspace integration outcomes](#verdict-gate)). Workers
   never push. A valid `LOOM_WAITING` slot is not merged or gated and its clone
   remains in place while blocker-aware `bd ready` schedules other work.
2. **Retry with context** — on in-session worker failure (or explicit agent
   self-report via `LOOM_RETRY`), retries with the prior error output injected
   as the `previous_failure` template variable. Configurable max retries per
   bead (default 2; `LOOM_RETRY` consumes one slot per emission). After
   in-session retries exhaust, the phase ends; the verdict is delegated to the
   [Verdict Gate](#verdict-gate).
3. **Verdict gate per phase** — worker sessions are classified by the verdict
   gate after the agent emits its terminal marker. Per-bead `loop` workers use
   the decision table above; review uses the review-specific terminal handling
   documented there. For implementation beads, the driver then runs
   deterministic post-integration verification
   (`loom gate verify --diff <pre-integration-head>..HEAD`) before the bead's
   integration is durable. LLM review is not part of the default per-bead hot
   path; the worker prompt requires self-review, and authoritative LLM review
   runs at molecule completion over the actual push range. See
   [Verdict Gate](#verdict-gate) for the execution layer (decision table,
   recovery mechanics, markers, labels) and [Gate](gate.md) for the review
   rubric. Driver-detected gate failures and `LOOM_RETRY` self- reports enter a
   bounded recovery loop; semantic `Blocked` goes to Inbox, while admitted
   `Clarify` records surface decision beads independently of terminal outcome.
   Worker completion reconciliation applies to Loop/Todo/Review, not interactive
   Plan/Inbox. Human resolution remains authoritative; deterministic resumption
   reconciles only the driver's explicitly attributed scheduling waits under
   [Inbox's ownership contract](inbox.md#orchestration-ownership).
4. **Push gate — consume the gate-owned receipt.** Loop owns the
   molecule-completion orchestration: require resolved molecule state under
   [completion admission](#completion-admission), not Beads progress alone;
   synchronize the integration branch with origin, resolve the actual push
   range, execute deterministic pre-push verification followed by review, and
   route integrity findings through molecule remediation. It then supplies the
   resulting handoff evidence to `GateSuccess::new`. The receipt's evidence set,
   matching rules, structural seal, and rejection conditions are owned only by
   [Gate — Gate success receipt](gate.md#gate-success-receipt). A rejected
   receipt becomes `GateOutcome::Fail` and refuses the push; an accepted receipt
   is the only path to marker minting and the clean push branch. Together with
   this workflow's epic worker filter, this keeps a clean epic close unreachable
   without a gate-authorized receipt.

   Under the command entrypoint, auto-iteration on promoted deferred remediation
   beads is owned by `loom loop`'s outer loop, bounded by
   `[loop] max_iterations`; this requirement is the molecule-final condition the
   outer loop drives toward, not a separate iteration mechanism.

   **Epic auto-close on Clean push.** After the `Clean` branch of the push gate
   completes (verify pass + review `LOOM_COMPLETE` + integrity clean + every
   bead in scope `[done]`) **and both `git push` and `wrix beads push`
   succeed**, the driver walks up from the molecule's spec beads and applies
   [fresh ancestor admission](#completion-admission) before each close. Only
   eligible epics with closed direct children are closed via
   `bd close <epic-id> --reason="all children complete; auto-closed by review gate"`.
   The walk is **inside-out in one pass**: each newly closed epic re-enqueues
   its parent for a fresh check. Eligible ancestors can retire in that pass;
   independent holds/cancellation and unresolved or ambiguous completion remain
   untouched, even outside the selected molecule. Each close emits one
   `DriverKind::EpicAutoClosed` driver event carrying the epic id in its payload
   — visible in the JSONL log alongside the push- gate trace. The walk is
   **strictly post-push**: an initial `git push` or `wrix beads push` failure
   returns early through the `Clean` arm and skips the walk. Subsequent epic
   close writes are new Beads mutations; the preceding synchronization does not
   establish their remote publication. The walk does **not** fire on any
   non-Clean verdict (`LOOM_CONCERN`, `LOOM_BLOCKED`, `verify-fail`,
   `integrity-finding`, or any non-closed bead carrying `loom:blocked` /
   `loom:clarify`) — those paths leave the gate before the `Clean` arm runs.

### Verdict-gate production wiring

**FR12 — Verdict-gate production wiring** — the verdict-gate decision function
is the single source of truth for marker → outcome routing. Production MUST
invoke it from `loom loop`'s per-bead exit and `loom gate review`'s phase-end;
no site may inline ad-hoc marker classification. The function is unit-tested in
isolation and also exercised through its production callers (live-path
coverage), per the trust-tier rules in
[docs/spec-conventions.md](../docs/spec-conventions.md).

### Observer-abort routing

1. **Observer-abort verdict-gate routing** — when an `EventSink::react()`
   returns `SessionCommand::Abort`, the driver cancels the session and
   classifies the outcome as recovery cause `observer-abort` with detail naming
   the responsible observer + the reason. This is the verdict-gate landing path
   for the loom-llm observer behavior owned by [Llm](llm.md) (notably
   `DoomLoopObserver`'s stage 2). Without this routing, observer kills would
   mis-classify as `swallowed-marker`.

### Retry context

1. **Typed `PreviousFailure`** — `LoopContext.previous_failure` is
   `Option<PreviousFailure>` where `PreviousFailure` is a tagged enum
   (`DriverNotice`, `VerifyFailures`, `ReviewConcern`, `BadWalk(BadWalk)`,
   `BuildFailure`, `TreeNotClean`, `PostIntegrateFail`, `IntegrationConflict`,
   `AgentRetry { reason: String }`). The driver populates the right variant from
   the verdict-gate cause classification. Each variant renders with distinct
   framing per _Typed `PreviousFailure`_ above. Caps:
   `PREVIOUS_FAILURE_MAX_LEN = 4000` total; per-block stderr tail ~1500 chars.
   Optional caller-supplied `review_notes` is outside this typed channel and its
   caller owns the budget. `AgentRetry.reason` shares the per-block budget cap.
2. **Attempt counter.** `LoopContext.attempt: u32` is the per-bead in-session
   retry counter, bounded by `[loop] max_retries` (default 2), resets to 0 on
   fresh bead dispatch. Fix-up beads start at `attempt = 0`; work-epic-level
   iteration is opaque to the agent. `loop.md` renders the attempt line when
   `attempt > 0 && previous_failure.is_some()`, omits it otherwise.

### Self-reports and dependency waiting

[Acceptance](#verdict-gate-1).

The shared [message and phase contracts](protocol.md#message-contract) define
encodings; this owner defines worker effects. `Retry` means a fresh dispatch is
likely to succeed and consumes the existing retry budget. `Blocked` means a
semantic dead end and its reason explains why safe candidate options cannot be
framed. `Clarify` reports durable decision references; `Waiting` ends an attempt
whose own work genuinely has unresolved prerequisites. A worker can report
decisions and still finish independent work.

Direct Loop/Todo guidance comes from `partial/self_report_markers.md` and
`partial/dependency_wait.md`. Review uses its inspection-only self-report
partial and sends decision-worthy concerns through clarify-route finding
evidence. Interactive sessions resolve friction conversationally rather than
emitting worker self-reports.
[Decision scheduling](#decision-batches-and-attributed-waits) and
[Inbox](inbox.md#decision-beads-and-resolution) own persistence and release, not
a new generic reconciliation rule.

### Dispatch and recovery coverage

- Profile/runtime selection from bead labels plus resolved backend (parse,
  fallback to base, flag override, missing runtime failure)
  - Interactive `plan` / `inbox chat` command-construction tests cover the
    backend-specific launch matrix owned by
    [Agent — Interactive Shell-Out](agent.md#interactive-shell-out)
  - Retry logic follows the [Verdict Gate](#verdict-gate): semantic recovery
    exhaustion applies `loom:blocked` with cause `retry-exhausted`, not an
    automatic clarification. Infrastructure failures retain their separate
    budget and `loom:infra` routing.
  - Push gate logic (clean completion, fix-up beads, iteration cap)
  - No per-bead `bd dolt push/pull` is invoked: assert `BdClient` exposes no
    `dolt_push`/`dolt_pull` methods and the workflow paths do not spawn
    `bd dolt …` subprocess calls (containers reach the authoritative state via
    the bind-mounted Dolt socket)
  - Parallel dispatch and integration tests execute the
    [Workspaces-owned bead lifecycle](workspaces.md#bead-dispatch) through
    public seams; production CLI coverage is used when command routing is the
    behavior under test

### Status command

- `loom status` prints active work epic, pending `loom:todo` work epic,
  iteration count, and cache health in a stable parseable format
  - `loom status` with no active work epic exits 0 with a clear message, not an
    error

## Out of Scope

- Verification policy and publication authority belong to Gate and its
  providers. This workflow does not invent a second authorization or retry path.
- An additional Beads publication/retry cycle solely for post-push epic closure
  writes. Initial publication ordering does not claim those later writes are
  already synchronized remotely.

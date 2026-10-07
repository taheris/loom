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
[gate](gate.md), [findings](findings.md), [templates](templates.md).

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

Worker terminal output is untrusted until reconciled with mechanical state.
Interactive `plan` and `inbox chat` sessions are human-authoritative and bypass
worker reconciliation; they accept only their phase-valid completion or apply
handoff. Review sessions use the finding/terminator protocol in [Gate](gate.md).

For a loop worker, the final marker, bead closure, branch diff, and tree state
produce the following result:

| Marker and state                                                            | Result                                                                            |
| --------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `LOOM_BLOCKED` with no safe options                                         | semantic block                                                                    |
| `LOOM_CLARIFY` with a persisted Options block                               | human clarification                                                               |
| `LOOM_RETRY` with a preceding reason                                        | bounded worker recovery                                                           |
| no valid final marker                                                       | `swallowed-marker` recovery                                                       |
| `LOOM_WAITING`, bead open, at least one active declared blocking dependency | typed dependency wait; preserve the bead workspace/branch and continue ready work |
| `LOOM_WAITING` with a closed bead or no active declared blocker             | `invalid-waiting` recovery                                                        |
| `LOOM_COMPLETE` while the bead is open                                      | `incomplete-signaling` recovery                                                   |
| `LOOM_COMPLETE` with an empty diff                                          | `zero-progress` recovery                                                          |
| successful marker with a dirty tree                                         | `tree-not-clean` recovery                                                         |
| `LOOM_COMPLETE`, closed bead, non-empty diff, clean tree                    | integrate and verify                                                              |
| `LOOM_NOOP`, closed bead, empty diff, clean tree                            | intentional no-work success                                                       |

`LOOM_CLARIFY` is valid only after the worker persists a well-formed
[Options Format Contract](inbox.md#options-format-contract) block on the target.
A missing or malformed block becomes `loom:blocked` with cause
`clarify-without-options`. `LOOM_RETRY` consumes the in-session retry budget;
`LOOM_BLOCKED` and valid clarification go directly to inbox. `LOOM_WAITING` is a
loop-only bare terminal whose durable payload is the Beads dependency graph: the
current bead remains open and at least one direct `blocks` dependency must
remain non-closed. A valid wait consumes no retry budget, performs no
integration or per-bead gate, mutates no workflow label/status, and leaves the
workspace for the blocker-aware ready queue to resurface after dependencies
close.

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
inside-out closure of ancestor epics whose direct children are closed.

Infrastructure failures remain distinct from semantic worker outcomes. Static
configuration/dispatch faults pause the bead as `loom:infra` without transport
retry. Spawn, handshake, transport, framing, or premature-stream failures use a
separate per-loop infra attempt budget and round-robin behind other ready work.
Exhaustion pauses the bead as `loom:infra`; a later loop invocation gets a fresh
budget. `first_event_seen` distinguishes pre-stream from interrupted sessions.

Every driver-detected recovery carries a typed `PreviousFailure`, bounded
attempt count, and durable evidence such as dirty paths, verifier failures,
conflict files, gate log path, or agent retry reason. Recovery exhaustion
preserves the cause in Beads. Remediation work is bonded to its originating
molecule before becoming dispatchable so molecule progress and push refusal see
all unresolved blocked, clarify, deferred, and infra state.

Marker ownership is phase-specific:

- `LOOM_COMPLETE` is generic success for loop, clean review, plan, and inbox;
- `LOOM_NOOP` is loop-only empty-diff success;
- `LOOM_WAITING` is loop-only typed dependency waiting, valid only against an
  open bead with an active declared blocker;
- `LOOM_TODO: <json>` is todo-only typed success;
- `LOOM_APPLY: {"proposals":[...]}` is inbox's trusted apply handoff;
- `LOOM_RETRY`, `LOOM_BLOCKED`, and direct `LOOM_CLARIFY` are worker
  self-reports subject to their phase restrictions; and
- `LOOM_CONCERN: {"summary":"..."}` terminates a review that streamed one or
  more `LOOM_FINDING:` records.

Exactly one phase-valid marker appears on the final non-empty line. Exit status
alone does not authorize state transitions.

### Loop Outcome Types

[Acceptance](#workflow-commands).

`LoopOutcome` and `GateOutcome` are architecture-bearing types: a successful
loop invocation cannot omit the push-gate result. `LoopOutcome` has no default,
is `must_use`, records processed, waiting, clarified, and blocked counts, and
carries a non-optional `GateOutcome`.

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
    /// escalates to `loom:clarify` with driver-authored Options.
    IntegrationConflict {
        files: Vec<PathBuf>,
        new_base_sha: GitOid,
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

### Dependency-Wait Terminal

[Acceptance](#verdict-gate-1).

`partial/dependency_wait.md` is included only by `loop.md`. It instructs the
worker to declare `bd dep add <current> <blocker>`, keep the current bead open,
and emit bare `LOOM_WAITING` on the final non-empty line. The marker has no JSON
payload because Beads is authoritative. It also states the mechanical outcome: a
valid wait preserves the worktree/branch, skips integration and per-bead gates,
consumes no retry budget, adds no blocked/clarify/infra state, and lets
blocker-aware `bd ready` resurface the bead. A closed bead or absent active
blocker is invalid and enters recovery.

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
  **refused** push (blocked/clarify/deferred/infra bead, pre-push failure,
  verify-fail, review-concern, integrity finding, or missing marker coverage)
  mints nothing. A missing or invalid marker falls the pre-push consumer through
  to running hooks rather than failing the push by itself
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
  exhaustion, the gate falls back to the terminal escalation: `loom:clarify` on
  the molecule's epic with one composed auto-generated `## Options     — …`
  block (kind-grouped resolutions per
  [Verify — Integrity gate](verify.md#integrity-gate))
  [test](push_gate_recovers_integrity_findings_until_cap_then_clarifies)

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
  cleanliness) without an LLM call
  [test](recovery_cause_labels_match_spec_strings)

<!-- prettier-ignore -->
- `phase_verdict::decide()` is invoked from `loom loop`'s per-bead exit AND from
  `loom gate review`'s phase-end; no production site inlines ad-hoc marker →
  outcome classification (FR12) [check](cargo run -p loom-walk -- phase_verdict_decide_called_from_production)

- `loom loop` never invokes `bd close` on a bead it dispatched; closure is the
  agent's responsibility and the `bd-closed` column is observed post-hoc.
  Verified by stubbing an agent that emits `LOOM_BLOCKED` / `LOOM_CLARIFY`
  without calling `bd close` and asserting the bead remains open after the run
  finishes.
  [test](loom_loop_never_invokes_bd_close_on_dispatched_bead_across_all_markers)

- `LOOM_BLOCKED` agent marker with a non-empty reason transitions the bead to
  `[blocked]` and skips the recovery loop
  [test](blocked_marker_routes_to_blocked_with_reason)

- `LOOM_CLARIFY` agent marker → bead transitions to `[clarify]`, recovery loop
  is skipped [test](clarify_marker_routes_to_clarify_with_question)

- Direct-emit `LOOM_CLARIFY` (`loop` / `todo` only): the gate validates the
  target bead/work epic's notes ∪ description for a well-formed
  `## Options — <summary>` heading with at least one `### Option <N> — <title>`
  subsection before applying `loom:clarify`. Same shape mint validates on a
  clarify-route finding's evidence. Forgetful- agent case (marker emitted,
  options block absent or malformed) falls back to `loom:blocked` with cause
  `clarify-without-options` — no stranded clarify bead reaches `loom inbox`
  [test](direct_emit_clarify_without_options_block_falls_back_to_blocked)

- Clarify downgrades emit `DriverKind::ClarifyDowngraded`, write a bd note
  breadcrumb with cause `clarify-without-options`, and pair the resulting bd
  label/status mutation with `DriverKind::BdStateTransition`
  [test](clarify_downgrade_emits_driver_events_and_bd_breadcrumb)

- `LOOM_RETRY` agent marker → recovery with cause `agent-retry`,
  `previous_failure` populated with `AgentRetry { reason }` from the prose
  preceding the marker; one `[loop] max_retries` slot consumed
  [test](agent_retry_consumes_max_retries_slot_and_threads_reason)

- `LOOM_RETRY` recovery exhaustion → `loom:blocked` with cause `retry-exhausted`
  (the same exhaustion path as other driver-detected recoveries)
  [test](consecutive_agent_retry_exhaustion_routes_to_loom_blocked_retry_exhausted)

- No marker emitted → recovery with cause `swallowed-marker`
  [test](missing_marker_routes_to_swallowed_marker_recovery)

- `LOOM_WAITING` is accepted only when the production loop observes the current
  bead still open with at least one active declared blocking dependency;
  acceptance leaves the bead open, preserves its workspace/branch, applies no
  workflow status/label, and runs no integration or per-bead gate
  [test](waiting_marker_preserves_open_bead_without_labels_or_integration)

- `LOOM_WAITING` without an active declared blocker is invalid, routes through
  visible recovery as `invalid-waiting`, and never silently parks the bead
  [test](waiting_marker_without_blocker_is_recovery_not_silent_parking)

- Parallel loop handling preserves the same waiting semantics: the waiting
  slot's workspace remains, its branch is not merged, and sibling ready work
  continues [test](parallel_waiting_outcome_preserves_workspace_without_merge)

- `LOOM_COMPLETE` + bead not bd-closed → recovery with cause
  `incomplete-signaling`
  [test](complete_without_bd_closed_routes_to_incomplete_signaling)

- `LOOM_COMPLETE` + closed + empty diff → recovery with cause `zero-progress`
  [test](complete_with_empty_diff_routes_to_zero_progress)

- `LOOM_NOOP` + closed + empty diff → accepted as intentional no-work output
  rather than zero-progress; no post-integration verify runs for an empty bead
  diff [test](run_bead_noop_empty_branch_is_done_not_zero_progress)

- `LOOM_COMPLETE` + closed + non-empty diff + dirty working tree
  (`git status --porcelain` non-empty) → recovery with cause `tree-not-clean`;
  post-integration verify is NOT run (recovery precedes it so verifiers don't
  execute against a half-staged tree); `previous_failure` lists the dirty paths
  capped at 30
  [test](complete_with_dirty_tree_routes_to_tree_not_clean_before_verify)

- `LOOM_NOOP` + closed + dirty working tree → recovery with cause
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

- The push gate walks `bd mol progress <id>` and refuses to push when any bead
  in the molecule — including bonded remediation beads — carries `loom:blocked`,
  `loom:clarify`, `loom:deferred`, or `loom:infra`; an orphan remediation bead
  would slip past this check, so the bond invariant is what makes the gate sound
  [test](remediation_beads_under_cap_auto_iterate)

- Recovery iter ≥ max_iterations → applies `loom:blocked` with cause in
  `bd update --notes`
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

- The push gate refuses to push while any bead in the molecule carries
  `loom:blocked`, `loom:clarify`, or `loom:infra`
  [test](clarify_or_infra_present_stops_without_pushing)

- Observer-driven abort (`EventSink::react()` returning `SessionCommand::Abort`)
  classifies as recovery cause `observer-abort` with detail naming the
  responsible observer + the reason it gave; distinct from `swallowed-marker`
  (which means the agent ended without a marker on its own, not under driver
  cancel)
  [test](observer_abort_routes_to_observer_abort_distinct_from_swallowed_marker)

- After the push-gate `Clean` branch's `git push` + `wrix beads push` both
  succeed, the driver walks the molecule's spec-bead parents and closes every
  ancestor epic whose direct children are all `status == "closed"` via
  `bd close --reason="all     children complete; auto-closed by review gate"`.
  Each close emits one `DriverKind::EpicAutoClosed` driver event carrying the
  epic id. [test](epic_auto_closes_when_all_children_closed_and_review_passes)

- Standalone review publishes Beads through the current `wrix beads push` CLI,
  never the removed `beads-push` helper or `bd dolt push`
  [test](beads_push_argv_invokes_wrix_beads_push_not_bd_dolt_push)

- Loop-owned molecule handoff publishes Beads through the current
  `wrix beads push` CLI, never the removed `beads-push` helper or `bd dolt push`
  [test](molecule_handoff_publishes_beads_through_wrix_cli)

- Epic auto-close does not fire while any direct child of the candidate epic
  carries `status != "closed"` (`open`, `in_progress`, or `deferred`).
  [test](epic_does_not_auto_close_when_any_child_non_closed)

- Epic auto-close does not fire on any non-Clean push-gate verdict
  (`LOOM_CONCERN`, any bead carrying `loom:blocked` or `loom:clarify`); only the
  `Clean` arm reaches the walk.
  [test](epic_does_not_auto_close_on_non_clean_review_verdict)

- Nested epics close inside-out in a single review-phase pass: closing an inner
  epic re-enqueues its parent so a fully- resolved grandparent retires in the
  same `Clean` walk. [test](nested_epics_close_inside_out_in_one_pass)

- Epic auto-close runs strictly **after** `git push` + `beads-     push`
  succeed; a push failure returns early through the `Clean` arm and skips the
  walk, so no closed-locally / open- on-remote split arises.
  [test](auto_close_skipped_when_git_push_fails)

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

- `PreviousFailure::AgentRetry { reason }` variant exists and carries the
  verbatim prose the agent wrote on the line preceding the `LOOM_RETRY` marker;
  populated by the driver when a worker phase exits with `LOOM_RETRY` per
  [Loop — Verdict Gate](#verdict-gate)
  [test](agent_retry_display_renders_reason_and_escalation_guidance)

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
  falls back to terminal `loom:clarify` on the molecule's epic with one composed
  auto-generated `## Options — …` block (kind-grouped resolutions per _Integrity
  gate_ above)
  [test](push_gate_recovers_integrity_findings_until_cap_then_clarifies)

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
   bounded recovery loop; `LOOM_BLOCKED` and direct loop/todo `LOOM_CLARIFY`
   self-reports escalate directly to the human via `loom inbox`. The verdict
   gate applies to **worker sessions only** (`loop`, `todo`, `review`);
   interactive sessions (`plan`, `inbox`) are agent-and-human authoritative —
   the driver does not mutate bd state as a consequence of an interactive
   session. See [Verdict Gate § Interactive vs worker sessions](#verdict-gate)
   for the full no-reconciliation contract.
4. **Push gate — consume the gate-owned receipt.** Loop owns the
   molecule-completion orchestration: require resolved molecule state,
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
   succeed**, the driver walks up from the molecule's spec beads to find every
   ancestor epic whose direct children are all `status == "closed"` and closes
   them via
   `bd close <epic-id> --reason="all children complete; auto-closed by review gate"`.
   The walk is **inside-out in one pass**: each newly-closed epic is enqueued so
   its own parent is re-evaluated, so an epic-of-epics collapses to a single
   closed root without needing a second review cycle. Each close emits one
   `DriverKind::EpicAutoClosed` driver event carrying the epic id in its payload
   — visible in the JSONL log alongside the push- gate trace. The walk is
   **strictly post-push**: a `git push` or `wrix beads push` failure returns
   early through the `Clean` arm and skips the walk, so a closed-locally /
   open-on-remote split cannot arise. The walk does **not** fire on any
   non-Clean verdict (`LOOM_CONCERN`, `LOOM_BLOCKED`, `LOOM_CLARIFY`,
   `verify-fail`, `integrity-finding`, or any bead carrying `loom:blocked` /
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

1. **Self-report marker taxonomy.** Direct loop/todo self-report markers form a
   three-way taxonomy carried by `partial/self_report_markers.md`:
   - `LOOM_RETRY` — this attempt cannot finish but a fresh dispatch is likely to
     succeed (environmental failure: tools failing mid-session, sandbox/cwd
     unlinked, transient IO; or agent self-reset: stuck-but-not-blocked,
     prompt-context exhausted). Consumes one slot in `[loop] max_retries`;
     exhaustion escalates to `loom:blocked` with cause `retry-exhausted` per
     [Loop — Verdict Gate](#verdict-gate). The driver populates
     `PreviousFailure::AgentRetry { reason }` with the prose the agent wrote on
     the line preceding the marker.
   - `LOOM_CLARIFY` — in `loop` and `todo`, the agent has framed a decision the
     human must resolve and can enumerate the candidate paths as a structured
     `## Options — …` block per
     [Inbox — Options Format Contract](inbox.md#options-format-contract). The
     agent persists the block to the bead/work epic before the marker, and the
     verdict gate routes to `loom:clarify` for human resolution via
     `loom inbox`.
   - `LOOM_BLOCKED` — genuine dead end: the agent cannot proceed and has no
     candidate resolutions to enumerate. The reason must explain why no options
     can be safely surfaced. Routes to `loom:blocked`; `loom inbox chat` walks
     the human through candidate enumeration in-session.

   Review uses `partial/review_self_report_markers.md` instead of the direct
   partial. The review partial preserves inspection-only review: it forbids bd
   mutation, treats direct `LOOM_CLARIFY` as the wrong review path, and sends
   clarify-worthy decisions through `route="clarify"` finding evidence with the
   canonical Options block. The semantic discriminator remains explicit: "expect
   retry to succeed? → RETRY. can you enumerate options? → CLARIFY (direct in
   loop/todo, finding-routed in review). dead end? → BLOCKED." Interactive
   sessions (`plan`, `inbox`) do not emit worker self-report markers — the human
   resolves friction in-turn. `inbox` may emit `LOOM_APPLY: {"proposals":[...]}`
   when it requests the trusted driver to apply accepted tune proposals.

2. **Dependency waiting in `loop`.** `partial/dependency_wait.md`, pinned only
   in `loop`, defines bare `LOOM_WAITING` for an open bead blocked by at least
   one active declared dependency. It directs the worker to add the Beads edge
   before emitting, forbids closing the current bead, and states that valid
   waiting preserves the workspace/branch while skipping integration, gate,
   retry-budget use, and workflow-state mutation. Invalid waiting enters
   recovery rather than parking.

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

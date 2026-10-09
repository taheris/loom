# Findings and remediation

Resolves untrusted finding records into immutable findings and materializes
scoped, bonded, deduplicated remediation.

## Problem Statement

Untrusted verifier and reviewer output must not create misattributed, duplicate,
or out-of-scope work. Findings resolves records before suppression, routing, and
remediation, retaining the evidence needed to repair the defect.

## Architecture

Raw records resolve into immutable findings; inspection reports them, while
act-mode consumers materialize scoped remediation. Related owners:
[gate](gate.md), [verify](verify.md), [specs](specs.md), [inbox](inbox.md),
[loop](loop.md), [harness](harness.md), [protocol](protocol.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Finding failure context

[Acceptance](#wire-format-strict-validation-and-max-context-preservation).

`BadWalk` preserves review diagnosis rather than replacing a failed walk with an
empty finding list. Its domain failures include:

- `Concern { payload, parsed_findings }`: malformed concern payload plus every
  successfully resolved finding.
- `ConcernWithoutFindings { summary }`: a concern terminal without findings.
- `FindingsWithoutConcern { finding_count, findings }`: findings paired with a
  clean terminal, retaining both count and records.
- `MalformedFinding { errors, terminal, parsed_findings }`: every malformed
  record's typed error, the independently established terminal surface, and
  every successfully resolved finding from the mixed stream.

The context fields are required, not optional defaults. Failure construction
cannot omit them; production conversion from the sealed walk retains their
actual contents. Field presence alone does not prove that a caller copied the
right records, so the behavioral matrix also checks complete transfer through
classification and bounded recovery rendering.

`TerminalSurface` is a diagnostic projection of Protocol's canonical decoded
terminal, or its malformed/missing surface. It owns no second marker vocabulary
or scanner. `FindingParseError` retains the record's starting line, literal raw
text, and typed decoding/resolution reason. Shared framing/phase errors likewise
retain the common decoder's context rather than being collapsed to empty output.

**Maximum-context preservation invariant.** Malformation never discards valid
resolved findings, raw error context, or an independently established terminal.
This includes mixed valid/invalid streams and simultaneous record/terminal
failure. See [the pairing contract](#emit-shape) and
[its matrix acceptance](#verification-surface-matrix--property).

The per-finding concern token (the enum that names which rubric check fired —
`verifier-bypass`, `spec-coherence-fail`, etc.) lives on each `Finding`'s
`token` field per
[Findings — Concern tokens and target variants](#concern-tokens-and-target-variants),
not on the `PreviousFailure::ReviewConcern` variant itself. The human-readable
label in the retry prompt is derived from the streamed findings:
`findings[0].token` when the token set is homogeneous, `multiple` when
heterogeneous, and `review-concern` only when the vector is empty. The terminal
marker is verdict-log shape only; per-finding routing is decided by
`loom gate mint`.

### Mint Default-Profile

[Acceptance](#mint-default-profile-1).

The driver-side `loom gate mint` flow labels fix-up and clarify beads with the
per-spec default profile: `profile:rust` for cargo-bound specs, so their
dispatch containers have the Rust toolchain needed by their verifiers;
`profile:base` for Nix-only and unknown specs. The operator can override the
label via `bd update <id> --labels` post-mint when a specific fix-up's toolchain
needs diverge from the spec's default.

`review.md` remains inspection-only and does not emit `bd create` calls; the
driver applies the default profile when minting from `LOOM_FINDING:` records.

### Remediation task acceptance

[Acceptance](#remediation-acceptance).

The driver constructs remediation goals from resolved findings, their batch
membership, targets, and evidence. It persists typed goal references through the
existing Beads boundary; descriptions and `finding:` labels alone cannot
manufacture resolved acceptance. At dispatch, the driver resolves a complete,
immutable goal set against the task and current workspace context. Raw records,
caller-supplied validation flags, missing records, and unrelated findings cannot
stand in for that set.

This goal is distinct from an implementation task's criterion assignments. A
finding about missing or malformed acceptance can be addressed without first
turning that broken criterion into a valid `CriterionStatus` or inventing a
CriterionId. Finding target resolution still applies: source locations and
malformation evidence must be real, rather than an exemption for arbitrary
unresolved references. Resolution failure is actionable and blocks dispatch;
changing the task kind is not an automatic fallback for bad criterion bindings.

Minted batches and remediation follow-ups, including worker-proposed splits,
retain explicit finding attribution through driver-owned persistence before
becoming dispatchable. Process the batch under the existing worker-discretion
contract: fix it, make permitted partial progress with explicit remaining work,
or seek clarification. Processing is not a claim that every finding was fixed,
that retained safety facts were resolved, or that publication is authorized.
[Loop](loop.md#task-acceptance-at-dispatch) owns the common dispatch boundary.

### Relocation and attribution

[Acceptance](#relocation-and-attribution-1).

Cross-label moves change label/anchor-dependent finding identities. Existing
finding, suppression, dedup, and status references require explicit
reconciliation, not automatic attachment by text similarity. Suppression must
not silently cover a different claim; known claim-attached negative evidence
retains linkage under Evidence. This transition does not introduce a permanent
legacy alias registry.

## Success Criteria

### Remediation acceptance

<!-- prettier-ignore -->
- Real minting, promotion, and remediation-split paths persist driver-resolved
  finding goals and deliver complete typed acceptance at dispatch, including a
  task repairing malformed acceptance without a fabricated criterion identity.
  Missing or forged goal references block before spawn; processed-batch
  acceptance preserves partial-work attribution and does not assert all
  findings resolved or grant publication authority. [system?](nix run .#test-quint -- remediation-acceptance)

### Standing-safety-net bonding

- `loom gate mint --tree` resolves each finding's bonded specs via typed `bonds`
  / target data, ensures exactly one spec epic exists for each bonded indexed
  spec (per
  [Todo — Spec and Work Epic Lifecycle](todo.md#spec-and-work-epic-lifecycle)),
  auto-creates and immediately closes missing metadata-only spec epics, and
  refuses duplicate spec epics before creating remediation work
  [test](mint_tree_scope_resolves_bonded_spec_epics_without_per_spec_work_epics)

- `loom gate mint --tree` creates exactly one remediation work epic for all
  actionable tree-scope batches in a run, parents every fix-up, blocked-clarify,
  and clarify bead under it, applies `loom:active` to that epic, clears
  `loom:active` from any previous work epic, names the active epic id and
  `loom loop` command in the summary, and writes no current-spec or
  pointer-table state
  [test](mint_tree_scope_mints_single_active_work_epic_for_all_actionable_batches)

- `loom gate mint --tree` creates no remediation work epic and leaves
  `loom:active` unchanged when no unsuppressed actionable finding remains after
  suppression, dedup, and structural validation
  [test](mint_tree_scope_without_actionable_findings_creates_no_epic)

- `loom gate audit --tree` is inspection-only: it walks the same rubric
  `mint --tree` walks and prints findings to stdout, but produces zero bd writes
  [test](audit_tree_scope_makes_no_bd_writes)

### Findings and Minting

- `loom gate mint` refuses to run when `LOOM_INSIDE=1`, exiting non-zero with a
  deterministic error message and producing no bd writes
  [test](mint_refuses_when_loom_inside_env_is_set)

- The walk emits `LOOM_FINDING: <json>` records on stdout, one JSON object per
  finding, streamed as findings are identified (not batched at end-of-walk). The
  JSON shape is
  `{"token": ..., "route": "blocking|deferred|clarify", "bonds": [...], "target": {"kind": ..., ...}, "evidence": ...}`
  [test](mint_walk_emits_loom_finding_json_lines_streamed_per_finding)

- Finding evidence preserves escaped multiline content through strict JSON
  decoding and contextual resolution; unescaped string newlines are rejected
  without repairing the emitted bytes.
  [test?](strict_finding_json_preserves_escaped_evidence_and_rejects_raw_newlines)

- Review consumes Protocol's review-admitted messages; retry/blocked terminals
  carry typed reasons and direct decision records are invalid. Decision-worthy
  review concerns use clarify-route finding evidence, and missing completion
  cannot authorize minting or completed review evidence.
  [test?](review_message_admission_preserves_inspection_and_requires_terminal)

- `LOOM_CONCERN:` payload parses as JSON `{"summary": "<non-empty string>"}` via
  the same `serde_json` pipeline that consumes `LOOM_FINDING:` records; the
  parsed summary becomes the verdict-log entry for the walk
  [test](concern_payload_parses_as_json_with_summary_field)

- Parse failures on the `LOOM_CONCERN:` payload — invalid JSON, missing
  `summary` field, empty `summary` string — surface as
  `RecoveryCause::BadWalk(BadWalk::Concern { payload, parsed_findings })`
  carrying the literal post-marker text so the recovery prompt can quote it back
  to the agent
  [test](concern_malformed_payload_routes_to_bad_walk_concern_with_literal_payload)

- A walk that emits `LOOM_CONCERN:` with zero preceding `LOOM_FINDING:` records
  surfaces as
  `RecoveryCause::BadWalk(BadWalk::ConcernWithoutFindings { summary })` —
  concern claimed without enumeration
  [test](concern_without_streamed_findings_routes_to_badwalk_concern_without_findings)

- A walk that streams one or more `LOOM_FINDING:` records and terminates with
  `LOOM_COMPLETE` surfaces as
  `RecoveryCause::BadWalk(BadWalk::FindingsWithoutConcern { finding_count, findings })`,
  retaining the streamed findings for recovery
  [test](findings_streamed_with_complete_terminator_routes_to_badwalk_findings_without_concern)

<!-- prettier-ignore -->
- The wire-format anti-drift verifier (a `[check]`-tier audit) scans every file
  under `crates/loom-templates/templates/` for the literal substrings
  `LOOM_CONCERN:` and `LOOM_FINDING:` and fails if they appear in any file other
  than `partial/findings_walk.md`. Bare-prose mentions without the colon (e.g.
  _"the `LOOM_CONCERN` marker"_) are unaffected [check](cargo run -p loom-walk
  -- template_wire_format_restatement)

- The anti-drift verifier accepts the canonical layout: with `LOOM_FINDING:` /
  `LOOM_CONCERN:` substrings present only in `partial/findings_walk.md`, the
  walk reports zero violations
  [test](anti_drift_verifier_passes_canonical_partial_layout)

- The anti-drift verifier fails a fixture where a template restates the
  wire-format outside the canonical partial — e.g. injecting `LOOM_FINDING:`
  into `review.md` directly — naming the offending file and line
  [test](anti_drift_verifier_fails_fixture_with_restated_wire_format)

- The driver parses `LOOM_FINDING:` JSON payloads via `serde_json` into typed
  `RawFinding` records, then resolves them into immutable `Finding` values. The
  `target` field deserializes as an internally tagged enum selected by `kind`;
  resolution checks its agreement with the token and current context
  [test](mint_parses_loom_finding_json_into_typed_record_with_tagged_target)

- A recognized finding record with malformed strict JSON, an unknown token or
  spec, token/target mismatch, or unresolved target fails with a typed error
  naming its starting line; no malformed record is silently skipped.
  [test?](shared_finding_decode_and_resolution_fail_with_record_context)

- The driver computes a versioned lower-kebab finding id from each validated
  typed finding; evidence text, options prose, line numbers, batch size, sibling
  batch membership, current bd parent, and `bonds` ordering do not affect the id
  [test](mint_computes_versioned_finding_id_excluding_volatile_context)

- The driver computes a compact finding hash from the finding id and refuses on
  hash collision instead of merging two different ids under one `finding:<hash>`
  label [test](mint_refuses_finding_hash_collision)

- `StyleRule` targets include a concrete subject in addition to `rule_id`; a
  rule-id-only style target is rejected as too broad for dedup or suppression
  [test](style_rule_finding_requires_concrete_subject)

- Mint dedups by `finding:<hash>` labels across live statuses; one live result
  skips that finding, zero live results allows minting, and more than one live
  result refuses the mint run as a structural violation
  [test](mint_dedups_per_finding_hash_label_across_live_statuses)

- Closed beads carrying `finding:<hash>` suppress automatic reminting only
  within the same owning molecule; closed matches outside the molecule are
  historical context only and do not suppress newly observed current findings
  [test](closed_finding_hash_label_suppresses_remint_only_within_same_molecule)

- A blocked clarify bead carrying `finding:<hash>` dedups the same clarify-route
  finding while it remains live, so unresolved `loom:clarify` decisions do not
  remint endlessly [test](blocked_clarify_bead_dedups_same_finding_hash)

- `[[suppress]]` entries in `loom.toml` require `reason` and exactly one of `id`
  (canonical finding id) or `hash` (compact finding hash); matching
  rubric-origin findings are reported as suppressed and removed from verdict /
  mint processing
  [test](loom_toml_suppress_entries_filter_rubric_findings_by_id_or_hash)

- Suppressions do not apply to deterministic or integrity findings; matching
  `[[suppress]]` ids or hashes for those findings are reported as ineffective
  and the findings still fail / mint normally
  [test](suppressions_do_not_filter_deterministic_or_integrity_findings)

- A well-formed `LOOM_CONCERN` walk whose every streamed finding is suppressed
  exits clean after emitting suppressed-status records; suppression does not
  forgive malformed stream / terminator pairing
  [test](all_suppressed_concern_walk_exits_clean_after_shape_validation)

<!-- prettier-ignore -->
- Inline code-comment suppressions are unsupported; the gate does not scan
  source comments for suppression directives [check](cargo run -p loom-walk --
  no_inline_suppression_comment_contract)

- `LOOM_FINDING_STATUS:` driver-emitted JSON lines carry each enriched finding's
  id, hash, bd label, token, target, and action without requiring the LLM to
  emit derived identity fields
  [test](driver_emits_finding_status_json_with_identity_and_action)

- At `--tree` scope, a live remediation bead whose `finding:<hash>` labels are
  all absent from the current unsuppressed finding set is reported as a stale
  candidate, not auto-closed
  [test](mint_tree_reports_stale_candidates_without_closing)

- At `--tree` scope, a live batch whose finding labels are partially stale is
  reported as a partially-stale candidate with current and absent finding ids,
  not auto-superseded
  [test](mint_tree_reports_partially_stale_batches_without_superseding)

- `-m/--molecule` promotion does not report stale candidates because molecule
  promotion consumes already-recorded deferred findings and cannot prove a
  missing finding is absent from the whole tree
  [test](mint_non_tree_scopes_do_not_report_stale_candidates)

- Each minted batch bead is parented under the scope-selected work epic (the
  molecule work epic for `-m/--molecule`, or the single standing remediation
  work epic for `--tree`) and carries one `finding:<hash>` label per contained
  finding plus one `spec:<X>` label per unique entry across the union of `bonds`
  over the batch's findings
  [test](mint_batches_parent_under_scope_selected_work_epic_with_union_spec_labels)

- The bonding lead is the first element of each finding's `bonds` array after
  validating unique spec epics for bonded specs. Findings sharing a lead may
  bundle into the same lead-spec remediation batch, but the lead affects
  grouping/labels only and does not create a per-spec parent epic for tree-scope
  minting; finding ids and hashes remain unchanged
  [test](mint_bonding_lead_groups_findings_without_selecting_tree_parent_epic)

- For target variants that carry a spec field (currently `Criterion` and
  `Invariant`), `target.spec` MUST appear in that finding's `bonds`; a finding
  that violates this is rejected with a typed parse error and the containing
  mint run is refused
  [test](mint_rejects_criterion_target_whose_spec_is_not_in_bonds)

- Clarify-bound findings mint as single-finding beads (one bead per
  clarify-route finding, not bundled into the spec's remediation batch) carrying
  `finding:<hash>` and `loom:clarify` labels, with the description embedding the
  `## Options — …` block extracted from the finding's `evidence` per the
  _Options Format Contract_
  [test](mint_clarify_bound_finding_creates_single_bead_with_finding_hash_label_and_options_block)

- Any clarify-route finding whose `evidence` lacks a well-formed `## Options —
  <summary>` heading with at least one `### Option <N> — <title>` subsection
  falls back to a remediation bead carrying `loom:blocked` with cause
  `clarify-without-options` — never a stranded clarify bead Inbox chat
  cannot resolve
  [test](mint_clarify_bound_finding_without_options_falls_back_to_blocked)

- Trusted finding materialization requires Inbox's unique active brief before
  human-queue admission, checking evidence and the resulting notes/description.
  Missing, malformed or duplicate active blocks use the per-decision
  `clarify-without-options` fallback. Inspection retains the finding and brief
  diagnostics without Beads mutation or loss of finding context.
  [test?](finding_clarification_materialization_requires_unique_active_brief)

- Tree-minted `loom:clarify` beads and their `loom:blocked` malformed-options
  fallbacks have `status=blocked` while awaiting `loom inbox`, so Beads visibly
  marks them as parked and excludes them from `bd ready`
  [test](mint_tree_clarify_children_are_blocked_pending_inbox)

- Fix-up batches enumerate every finding in the bead description (one item per
  finding: finding id, hash, token, target's canonical form, evidence excerpt);
  the title is stable across runs for the same contained finding-hash set
  [test](mint_batch_description_enumerates_finding_identity_and_title_is_stable)

- A remediation batch carrying multiple findings exposes worker discretion to
  fix all and close, fix a subset and split the remainder into sibling
  remediation beads under the work epic via `bd create --parent=<work-epic-id>`
  for deferred work; the bead's acceptance criterion is "agent processed the
  batch", not "every finding individually resolved". Human-decision reporting
  and genuine source waits follow
  [Loop's decision contract](loop.md#decision-batches-and-attributed-waits)
  [judge](../tests/judges/loom.sh#judge_remediation_batch_acceptance) Per-bead
  integration execution mechanics are owned by
  [Loop — Verdict Gate](loop.md#verdict-gate);
  [Gate](gate.md#gate-success-receipt) owns the receipt consumed at that
  boundary.

- `mint --tree` walks both the deterministic verifiers and the LLM rubric,
  normalizing findings from either source into the same typed mint flow
  [test](mint_tree_scope_walks_verifiers_and_rubric_emitting_findings_from_both)

- Mint is idempotent against partial failure: a crash mid-run leaves
  successfully-minted findings with their `finding:<hash>` labels; a re-run's
  per-finding dedup query skips those hashes and retries only the unfinished
  findings
  [test](mint_idempotent_after_partial_failure_retries_only_unfinished_findings)

- `mint --tree` never leaves an open active remediation work epic with zero
  child beads: if the epic was created but no child bead was created before
  failure, the driver closes or neutralizes the epic and restores the
  `loom:active` bookmark to its pre-run state; if at least one child bead was
  created, the non-empty epic remains open/active and rerun dedups the completed
  children [test](mint_tree_partial_failure_never_leaves_empty_active_epic)

- `mint --dry-run` prints proposed bd writes and makes zero bd writes; at
  `--tree` it still walks the rubric/verifiers, and at `-m/--molecule` it
  previews deferred-promotion changes [test](mint_dry_run_makes_no_bd_writes)

- `mint` rejects `--spec`; lead-spec routing comes only from each finding's
  typed `bonds` and target after multi-spec lead selection
  [test](mint_rejects_spec_filter)

- Bare `loom gate mint` prints subcommand help and runs nothing; callers choose
  `--tree` or `-m/--molecule` explicitly
  [test](mint_bare_invocation_requires_explicit_scope)

- The end-of-run summary lists blocking findings, deferred findings merged,
  deferred beads promoted, ready remediation batches created or updated, clarify
  findings raised, skipped live finding hashes, suppressed rubric findings,
  stale candidates, partially-stale candidates, refused structural conflicts,
  and transient errors, with `LOOM_FINDING_STATUS:` JSON carrying per-finding
  details [test](mint_end_of_run_summary_reports_finding_lifecycle_outcomes)

### Review finding wire contract

- Review concerns are represented only by parsed `LOOM_FINDING:` records plus
  their terminal summary; no second concern-without-findings error path is
  available to production callers
  [test](no_path_constructs_concern_without_bead_deltas_in_production)

- Review classification accepts typed finding records rather than parsing a
  free-form token-and-reason line from whole stdout
  [test](streamed_findings_reach_recovery_without_legacy_flags)

- An unrecognized terminal summary accompanied by at least one typed finding
  routes to `RecoveryCause::ReviewConcern { summary, findings }`, not
  `SwallowedMarker`
  [test](decide_concern_unrecognized_summary_with_findings_routes_to_review_concern_not_swallowed)

- `ReviewConcern` is display vocabulary for human-readable notes and verdict
  logs. `PreviousFailure::ReviewConcern` derives its label from the finding
  tokens, using a multiple-findings label when the tokens differ
  [test](previous_failure_review_concern_renders_human_label_from_findings_not_summary)

### Wire-format strict validation and max-context preservation

- Mixed finding failures retain every successfully resolved finding alongside
  each raw/typed record error and the independently established terminal,
  including when that terminal is itself malformed or missing.
  [test?](malformed_finding_recovery_retains_valid_findings_errors_and_terminal)

- Fenced, decorated, inline, or payload-contained finding examples cannot
  substitute for live review records; review pairing uses only messages
  recognized by the shared decoder.
  [test?](review_pairing_does_not_promote_examples_or_payload_marker_text)

- `BadWalk::Concern` carries `{ payload, parsed_findings: Vec<Finding> }`;
  well-formed findings streamed ahead of a malformed terminal are preserved in
  `parsed_findings`
  [test](bad_walk_concern_preserves_well_formed_findings_alongside_malformed_payload)

- `BadWalk::FindingsWithoutConcern` carries
  `{ finding_count, findings: Vec<Finding> }`; the parsed findings ride through
  so the next iteration's prompt and `loom gate mint` can both consume them
  [test](bad_walk_findings_without_concern_carries_parsed_findings_vec)

- `BadWalk::MalformedFinding` requires errors, terminal context, and
  `parsed_findings`; the mixed-stream diagnosis cannot be omitted by
  constructing the failure without its context fields.
  [test?](malformed_finding_failure_requires_parsed_context_fields)

### Verification surface (matrix + property)

- The review-phase classifier signature consumes a typed `WalkOutput` (with
  field-private struct and public construction through the decode/resolve
  boundary), not raw `&str`. Any production caller passing a `&str` is a compile
  error, and any caller constructing `WalkOutput` with bogus fields cannot
  compile because the fields are private at the `loom-protocol` crate boundary
  [test](classify_review_phase_signature_requires_typed_walk_output)

- The review failure matrix crosses empty, valid, mixed, and wholly malformed
  finding streams with admitted, wrong-phase, malformed, absent, duplicate, and
  misplaced terminal surfaces. Every cell checks the typed outcome and all
  parseable context through actual classification and recovery rendering.
  [test?](review_failure_matrix_preserves_all_shared_decoder_context)

- Every constructible `Finding` (each `ConcernToken` × canonical `FindingTarget`
  combination) round-trips byte-equal through `serde_json::to_string` → embed in
  a `LOOM_FINDING:` record → embed in a synthetic walk output →
  `parse_walk_output`, with stable finding id and hash
  [test](every_finding_round_trips_through_wire_format_with_stable_identity)

- `ConcernToken::CrossSpecClash` round-trips through the wire format with
  canonical target `Criterion { spec, anchor }` and is exercised by the
  round-trip property test cell set
  [test](concern_token_cross_spec_clash_round_trips_with_criterion_target)

- `ConcernToken::SpecConventionsViolation` round-trips through the wire format
  with canonical target `Criterion { spec, anchor }` and is exercised by the
  round-trip property test cell set
  [test](concern_token_spec_conventions_violation_round_trips_with_criterion_target)

- `cross-spec-clash` and `spec-conventions-violation` are tree-scope-only
  tokens: the rubric emits them at `--tree` scope; `--diff` / `--files` scope
  rejects them. A finding carrying either token parsed at non-tree scope
  surfaces a typed `FindingParseError` variant naming the scope mismatch,
  alongside the existing diff-context restriction on `scope-creep` /
  `scope-shortfall` [test](tree_scope_only_tokens_rejected_at_non_tree_scope)

### `loom-protocol` crate

- `loom-templates::finding` and `loom-templates::previous_failure` re- export
  the typed contract from `loom-protocol::gate` via `pub use` so existing
  callers (including `PreviousFailure::ReviewConcern { findings }`) compile
  without changes. The original definitions are removed from `loom-templates`
  [test](loom_templates_re_exports_finding_contract_from_loom_protocol)

- Review entry points and re-exports compose the canonical output decoder with
  domain finding resolution; a separate gate/workflow terminal enum or scanner
  cannot bypass the common message contract.
  [test?](review_domain_entry_points_delegate_to_shared_output_decoder)

- The `WalkOutput` struct's fields are private; `WalkOutput::from_stdout` is
  `pub` (consumers need to call it) but is the only construction path. The
  silent-loss failure class — production caller constructs `WalkOutput` with
  bogus fields, bypassing the typed parse pipeline — is structurally
  unrepresentable via field-privacy, not via `pub(crate)` constructor scoping
  [test](walk_output_fields_private_only_constructor_is_from_stdout)

<!-- prettier-ignore -->
- The `finding_no_duplicate_definitions` walker continues to enforce one
  canonical definition of `Finding`, `ConcernToken`, `FindingTarget`,
  `WalkOutput` and `BadWalk` across the workspace; their canonical domain home
  is `loom-protocol::gate` [check](cargo run -p loom-walk
  -- finding_no_duplicate_definitions)

### Production walker wiring

- A production `MintWalker` implementation exists in `loom-workflow::mint::walk`
  (alongside the trait). Its `run_rubric` spawns the reviewer agent subprocess
  against the rendered review prompt and returns the agent's combined stdout;
  its `run_verifiers` dispatches the deterministic verifier set + the integrity
  gate forward-resolution check and returns one `VerifierFailure` per failed
  dispatch outcome. Both methods are used only for `MintScope::Tree`
  [test](production_mint_walker_exists_and_dispatches_rubric_and_verifiers)

- `run_gate_mint` in the loom CLI binary dispatches by scope: `--tree`
  constructs production walker(s) and obtains findings only through the
  `MintWalker` trait (`run_verifiers` once for the tree, then `run_rubric` per
  walked spec) before passing collected findings to the minting pipeline. A
  verifier/rubric source failure records an error in the mint summary and exits
  non-zero, but does not discard findings already collected from other specs;
  stale-candidate reporting is suppressed for that incomplete tree walk.
  `-m/--molecule` calls the deferred-promotion path and never constructs a
  placeholder empty findings vector
  [test](run_gate_mint_dispatches_tree_through_walker_and_molecule_through_promotion)

Per-bead routing and subprocess policy are owned by
[Loop — Verdict Gate](loop.md#verdict-gate). Findings owns finding-record
resolution and remediation, not verification-result admission or publication
trust. Those boundaries belong to
[Evidence](evidence.md#cache-and-evidence-reuse) and
[Gate](gate.md#gate-success-receipt), respectively.

### Molecule mint summary semantics

- `loom gate mint -m/--molecule <id>` exits 0 when it successfully promotes zero
  or more deferred remediation beads; the summary lists promoted counts and any
  reobserved closed findings
  [test](mint_molecule_exits_zero_on_successful_promotion_summary)

- `loom gate mint -m/--molecule <id>` exits non-zero when promotion sees a
  structural conflict (duplicate live finding hashes, missing work epic, or bd
  write failure), and the summary names the conflicting bead ids or bd error
  [test](mint_molecule_exits_nonzero_on_structural_or_write_errors)

Loop-side interpretation of these exit codes — retrying transient promotion
errors or blocking on structural bd state — is owned by
[Loop — Verdict Gate](loop.md#verdict-gate).

### Workflow commands

- `loom gate mint --tree` creates one standing remediation work epic for all
  actionable tree-scope fix-up / blocked-clarify / clarify beads in the run,
  parents every child under that epic, applies `loom:active` to it, clears
  `loom:active` from any previous work epic, and prints the epic id plus the
  follow-up `loom loop` command
  [test](mint_tree_sets_single_active_remediation_work_epic)

- `loom gate mint --tree` creates no work epic and leaves `loom:active`
  unchanged when no actionable child bead remains after suppression, dedup, and
  structural validation
  [test](mint_tree_no_actionable_findings_leaves_active_unchanged)

- `loom gate mint --tree` never returns with an open active remediation work
  epic that has zero child beads; failure before the first child closes or
  neutralizes the epic and restores `loom:active` to its pre-run state, while
  failure after at least one child leaves the non-empty epic open/active for
  dedup-friendly rerun
  [test](mint_tree_never_leaves_empty_active_remediation_epic)

- `loom loop [OPTIONS] [BEAD_OR_EPIC_ID ...]` runs the sole `loom:active` work
  epic when no ids are provided. Positional ids may be task beads (run exactly
  that bead) or epics (run ready child work under that epic/molecule). Options
  may appear before, between, or after ids. `--host-key` explicitly opts into
  ambient host Git credentials; `--spec`, `--once`, and `--all-specs` are not
  part of the loop surface
  [test](loop_accepts_positional_work_roots_and_defaults_to_active_epic)

### Mint default-profile

- The driver-side `loom gate mint` defaults cargo-bound specs to `profile:rust`
  [test](default_profile_for_spec_returns_rust_for_cargo_bound_specs)

- Nix-only / unknown specs fall through to `profile:base`
  [test](default_profile_for_spec_returns_base_for_nix_only_specs)

- Mint applies the resolved default profile as a `profile:<name>` label on every
  fix-up and clarify bead it creates; the operator overrides via
  `bd update <id> --labels` post-mint
  [test](mint_applies_per_spec_default_profile_label_to_created_beads)

### Typed `PreviousFailure`

- Every domain `BadWalk` shape requires the context fields defined by the
  finding-failure contract; actual conversion preserves valid records rather
  than supplying an empty default after mixed failure.
  [test?](bad_walk_failure_shapes_require_all_parseable_context)

- Review terminal diagnostics project the canonical decoded message or retain
  malformed/missing raw context without duplicating the terminal vocabulary or
  decoding path.
  [test?](review_terminal_diagnostics_project_canonical_output_messages)

### Canonical contract location

- Deterministic failure normalization resolves the declared annotation even when
  its executable does not resolve.
  [test](deterministic_failures_resolve_declared_not_executable_annotations)

- Serializing and resolving findings preserves their canonical identity and wire
  payload. [test](wire_roundtrip_preserves_canonical_identity_and_payload)

- Deserialized RawFinding values must resolve token, target, scope, and bonds
  before entering the immutable Finding boundary.
  [test](deserialized_raw_input_cannot_bypass_token_target_checks)

<!-- prettier-ignore -->
- Inspection commands cannot reach Beads mutation; only the driver-owned mint
  and explicit workflow act paths materialize findings. [check](cargo run -p loom-walk -- audit_makes_no_bd_writes_outside_mint_module)

<!-- prettier-ignore -->
- Finding wire types have one definition in loom-protocol and are re-exported
  rather than independently redefined. [check](cargo run -p loom-walk -- finding_no_duplicate_definitions)

### Relocation and attribution

- Cross-label moves require explicit reconciliation of finding and suppression
  references, retain negative-evidence attribution, and cannot silently suppress
  a new claim using an old identity.
  [test?](finding_relocation_preserves_attribution_without_implicit_suppression)

## Requirements

#### Findings and Minting

[Acceptance](#findings-and-minting).

`loom gate mint` is the only gate CLI command that materializes findings as
remediation Beads: tree scope walks and materializes; molecule scope promotes
already-recorded deferred work. Trusted Loop orchestration also records and
routes molecule-review findings through this owner's materialization boundary.
Every other gate subcommand is inspection-only with respect to Beads. The
[inspection/act partition](#inspection-vs-act-partition) distinguishes command
inspection from trusted driver effects.

##### Canonical contract location

[Acceptance](#canonical-contract-location).

`loom-protocol::gate` owns the public domain finding contract: `RawFinding`,
immutable `Finding`, `ConcernToken`, tagged `FindingTarget`, `TargetKind`,
`FindingValidator`, contextual errors, `BadWalk`, diagnostic terminal context,
and sealed `WalkOutput`. Public domain entry points such as `parse_walk_output`
and `WalkOutput::from_stdout` compose
[Protocol's canonical output contract](protocol.md#architecture) with finding
resolution. They do not define another terminal enum, marker registry, raw
scanner, or tolerant JSON dialect.

Consumers may inspect typed records, resolve raw candidates, and read parsed
walks. Internal resolution and identity helpers stay private; diagnostic
projections do not acquire authority to construct resolved findings. Public
re-exports retain one definition rather than another independently maintained
contract.

**The seal is field-private, not constructor-private.** `WalkOutput`'s private
fields prevent external struct-literal construction and field mutation;
`WalkOutput::from_stdout` is the public construction path. This enforces entry
through decoding/resolution, not correct input selection or downstream copying.
Actual context preservation follows the
[structural boundary and behavioral evidence](#structural-enforcement).
`Finding` also has private fields and borrowed read-only accessors. Untrusted
input constructs `RawFinding`; resolution produces the immutable `Finding`
consumed by review and mint. Editing its raw projection requires resolution
again; public mutable fields cannot bypass that boundary. The
[resolved finding boundary](#resolved-finding-boundary) governs construction.
Crate dependencies and wire versioning follow
[Harness](harness.md#canonical-contract-location).

**Cross-repo consumers.** External consumers (e.g. wrix) depend on
`loom-protocol` directly. They capture a gate result handoff and invoke the
public domain adapter with the declared scope and workspace validator. Shared
message decoding precedes contextual finding resolution; driver-authored status
records are a separate output surface, not agent findings. Consumers use the
same sealed walk and resolved finding boundary as Loom's own pipeline.

The finding-definition walk enforces one domain definition. Agent-facing review
presentation lives in `partial/findings_walk.md`; its restatement audit prevents
duplicate template prose, not disagreement with Rust parsing. Actual
prompt/parser agreement is checked through
[Templates' rendered conformance](templates.md#agent-output-conformance). Cargo
release alignment alone does not establish that agreement.

**Routing versus display.** `ConcernToken` is each finding's closed wire
identifier and participates in finding routing. `ReviewConcern` is only a
human-readable cause vocabulary for notes and verdict logs; it is neither a
message discriminator nor routing authority. Protocol's `Concern` terminal
carries a summary, while each finding retains its own token and route.

##### Inspection vs. act partition

[Acceptance](#findings-and-minting).

Every gate subcommand except `loom gate mint` is **inspection-only with respect
to Beads**: it may inspect or verify state and report results but performs no
`bd` writes. Among gate CLI commands, only `mint` enters the Findings-owned
materialization boundary. Trusted workflow orchestration can also enter that
boundary; this does not authorize inspection commands or reviewing agents to
mutate Beads. The partition is structural: no code path inside `loom gate audit`
/ `verify` / `review` / `judge` / `rubric` / `check` / `test` / `system` /
`verify-marker` may call the pipeline's `bd` write surface as a side-effect. The
source verifier enforces this separation while permitting trusted Loop routing
into `loom-workflow::mint`.

The driver's `loom loop` is an **operator-level composition** around the gate,
not a side-effect of an inspection subcommand. Its per-bead path runs
deterministic `verify --diff <pre-integration-head>..HEAD` after integration and
records a typed gate log without materializing findings. The molecule-completion
push gate composes pre-push deterministic verification with
`review --diff <actual-push-range>`. That review command remains read-only with
respect to Beads; the driver then resolves and routes its findings through
[deferred remediation processing](#deferred-remediation-processing), recording
or updating remediation and clarify work as required before any later promotion.
Stabilization's `loom gate mint -m <molecule>` promotes already-recorded
deferred beads rather than being their first persistence step.
[Loop — Verdict Gate](loop.md#verdict-gate) owns when these operations run; this
spec owns their materialization rules.

The `MarkerProof` mint at the molecule-completion push gate (see
[Gate — Marker](gate.md#marker)) is a **separate** mint surface, owned by
`loom-gate::marker` with its own `pub(crate)` constructor — it writes a single
content-addressed JSON file to `.loom/marker.json`, never bd state. "Audit makes
no bd writes" remains true through that path; the marker is filesystem state,
not bd state.

##### Shared output contract

[Acceptance](#loom-protocol-crate).

[Protocol](protocol.md#message-contract) owns message role, unit/data arity,
framing, and phase admission. This owner defines finding payloads, contextual
resolution, pairing, identity, and remediation. Review diagnostic types are
projections of that common contract, not another independently parsed language.

##### Scope-dependent walk

[Acceptance](#findings-and-minting).

`mint` is the act surface, so its scopes are intentionally narrower than
inspection commands:

| Scope                   | Walks / consumes                                                                                                                                                            | Why                                                                                                                         |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `-m`, `--molecule <id>` | Consumes the molecule's already-recorded deferred finding beads and promotes them to ready remediation work. It does not run new verifiers or review.                       | Stabilization is molecule-local; original implementation work drains before deferred findings become ready.                 |
| `--tree`                | Deterministic verifiers + LLM rubric over the full workspace, then creates or updates ready remediation batches under one active work epic when actionable findings remain. | Standing safety-net runs have no current push or worker session to recover into; deterministic failures have no other home. |

`mint` rejects `-b/--bead`, `--diff`, `--files`, `--target`, and `--spec`; those
scopes are inspection-only (`review` / `audit`) or deterministic-only
(`verify`). Bare `loom gate mint` prints subcommand help and runs nothing;
callers choose `--tree` or `-m/--molecule` explicitly.

##### LOOM_INSIDE guard

[Acceptance](#findings-and-minting).

`loom gate mint` refuses to run when `LOOM_INSIDE=1`. Deterministic gate
inspection subcommands may run inside a bead container for self-check and local
diagnostics. `mint` remains banned because bd writes from inside a loom-managed
container would mutate driver-owned state rather than act as a local inspection.
LLM-spawning gate subcommands follow the harness-level `LOOM_INSIDE` guard. The
check is a deterministic precondition; no walk runs and no exit code 2 path
fires.

##### Concern tokens and target variants

[Acceptance](#verification-surface-matrix--property).

Every finding carries a typed `target` whose variant is determined by the
`token`. The driver canonicalizes the variant when computing the finding id
(under _Finding id, finding hash, suppression, and dedup_ below) so the same
finding hashes the same way across rubric runs. Rubric-origin findings also
carry the explicit `route` field from _Worker and per-bead integration checks_;
the table below names the default route when no push-range classification
applies. At tree scope, `route="deferred"` still materializes ready remediation
because `loom gate mint --tree` is an explicit standing-safety-net act; a stray
`route="blocking"` from the tree rubric is accepted as the same ready
remediation for compatibility.

| Token                                                                            | Source                                                                                                                                                                                                                      | Target variant                                                                                        | Default route                                                                                                                    |
| -------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `spec-coherence-fail`                                                            | Rubric (conformance trace)                                                                                                                                                                                                  | `Criterion { spec, anchor }`                                                                          | deferred                                                                                                                         |
| `orphan-integration`                                                             | Rubric (contract closure)                                                                                                                                                                                                   | `Contract { id }`                                                                                     | deferred                                                                                                                         |
| `style-rule-violation`                                                           | Rubric (style-rule walk)                                                                                                                                                                                                    | `StyleRule { rule_id, subject }`                                                                      | deferred                                                                                                                         |
| `verifier-bypass` / `weak-assertion` / `fabricated-result` / `coincidental-pass` | Rubric (verifier-honesty walk)                                                                                                                                                                                              | `Annotation { target_string }`                                                                        | deferred                                                                                                                         |
| `mock-discipline`                                                                | Rubric                                                                                                                                                                                                                      | `TestPath { path }`                                                                                   | deferred                                                                                                                         |
| `verifier-too-narrow`                                                            | Rubric                                                                                                                                                                                                                      | `Criterion { spec, anchor }`                                                                          | deferred                                                                                                                         |
| `concurrency-untested`                                                           | Rubric                                                                                                                                                                                                                      | `LockSite { file, line }`                                                                             | deferred                                                                                                                         |
| `scope-creep` / `scope-shortfall`                                                | Rubric (finite-diff scope only)                                                                                                                                                                                             | `Criterion { spec, anchor }`                                                                          | deferred                                                                                                                         |
| `judge-flag`                                                                     | Rubric (`[judge]` criterion)                                                                                                                                                                                                | `Criterion { spec, anchor }`                                                                          | deferred                                                                                                                         |
| `invariant-clash`                                                                | Rubric (invariant-clash scan)                                                                                                                                                                                               | `Invariant { spec, section, tag }`                                                                    | **clarify** (evidence MUST embed `## Options — …`; mint falls back to blocked otherwise — see _Deferred remediation processing_) |
| `template-spec-drift`                                                            | Rubric (tree-scope only)                                                                                                                                                                                                    | `Template { path }`                                                                                   | deferred                                                                                                                         |
| `cross-spec-clash`                                                               | Rubric (tree-scope only)                                                                                                                                                                                                    | `Criterion { spec, anchor }`                                                                          | deferred                                                                                                                         |
| `spec-conventions-violation`                                                     | Rubric (tree-scope only)                                                                                                                                                                                                    | `Criterion { spec, anchor }`                                                                          | deferred                                                                                                                         |
| `verifier-failed`                                                                | Deterministic verifier exit ≠ 0 (tree-scope only)                                                                                                                                                                           | `Annotation { target_string }`                                                                        | deferred                                                                                                                         |
| `dispatch-error`                                                                 | Verifier exit 2 — command not found / missing prereq (tree-scope only)                                                                                                                                                      | `Annotation { target_string }`                                                                        | deferred                                                                                                                         |
| `unresolved-annotation`                                                          | Integrity gate forward-resolution (tree-scope and push-gate scope)                                                                                                                                                          | `Annotation { target_string }`                                                                        | deferred                                                                                                                         |
| `stub-pointing`                                                                  | Integrity gate stub-pointing (tree-scope and push-gate scope)                                                                                                                                                               | `Annotation { target_string }`                                                                        | deferred                                                                                                                         |
| `unneeded-pending-marker`                                                        | Integrity gate stale pending modifier (tree-scope and push-gate scope)                                                                                                                                                      | `Annotation { target_string }`                                                                        | deferred                                                                                                                         |
| `inputs-protocol-error`                                                          | Integrity/verification provider-query admission — a required description or observation query exited non-zero or emitted malformed metadata, including during local feedback                                                | `Annotation { target_string }`                                                                        | deferred                                                                                                                         |
| `multiple-annotations`                                                           | Integrity gate atomic-acceptance (tree-scope only)                                                                                                                                                                          | `Criterion { spec, anchor }`                                                                          | deferred                                                                                                                         |
| `pending-marker-resolved`                                                        | Sweeping walker (any scope) — a pending element (`?` or `~`) in structured spec input has resolved against the pending direction (`?` + present, or `~` + absent), so the author must drop the marker to its resolved value | `MatrixCell { spec, partial, template }` / `SurfaceElement { spec, kind, name }` / per-walker variant | deferred                                                                                                                         |

**Clarify-route subset.** Today only `invariant-clash` defaults to `clarify`;
the rest default to `deferred`. At push-range review the explicit `route` field
may classify any non-clarify token as `blocking` or `deferred` depending on
whether it invalidates the pushed work or identifies broader drift. Adding a
future default-clarify token is a one-row table edit + the new token's enum
entry; no per-token carve-out in the mint pipeline.

`scope-creep` and `scope-shortfall` are finite-diff review tokens; the
tree-scope walk does not emit them, and mint never receives them from a
tree-scope source.

Target variants encode their required payload shapes. Token/target alignment is
established by `RawFinding::resolve` before constructing an immutable `Finding`;
private fields prevent later mutation from bypassing that check. The target enum
alone does not prove the relationship. See the
[resolved finding boundary](#resolved-finding-boundary) and
[`spec-conventions.md` _In scope #4_](../docs/spec-conventions.md).

##### Emit shape

[Acceptance](#findings-and-minting).

The LLM rubric walk emits findings as streaming records on stdout from the
agent's subprocess. Each record starts with a `LOOM_FINDING:` prefix followed by
a JSON payload:

```
LOOM_FINDING: {"token":"<token>","route":"blocking|deferred|clarify","bonds":["<spec>",...],"target":<target>,"evidence":"<evidence>"}
```

- **`token`** — concern identifier from the closed-set enum in _Concern tokens
  and target variants_ above.
- **`route`** — rubric-origin workflow route. `blocking` refuses the current
  push-range review and creates or reuses same-molecule remediation work,
  `deferred` merges into a `loom:deferred` remediation bead for molecule
  stabilization, and `clarify` materializes a human-decision bead with options.
  Tree-scope rubric output should emit `deferred` for mechanical remediation and
  `clarify` for human decisions; if it emits `blocking`, the parser keeps the
  finding and tree mint materializes it as ready remediation. Tree-scope
  deterministic findings normalized by the driver do not come from LLM wire
  output; the driver assigns `deferred` at molecule/push scope and materializes
  ready remediation directly at tree scope.
- **`bonds`** — array of spec labels the remediation should bond to. Always
  present, always at least one element. The driver picks the bonding lead from
  this array via _Multi-spec findings_ below.
- **`target`** — tagged JSON object whose `kind` discriminator selects the
  variant per the table above; carries identity-bearing fields specific to the
  variant.
- **`evidence`** — the rubric's reasoning, stored verbatim on the remediation
  bead's description or verdict/recovery context. For `route="clarify"`,
  evidence MUST embed one canonical brief under
  [Inbox's Options contract](inbox.md#options-format-contract). Trusted
  materialization checks brief validity/uniqueness before human-queue admission;
  absent, malformed or ambiguous briefs use the `clarify-without-options`
  blocked fallback on that decision. Shared decoding and finding resolution
  retain the record; inspection reports the diagnosis without Beads writes. See
  _Deferred remediation processing_ below.

`bonds` is _bonding_ metadata; `target` is _identity_ metadata. The two are kept
separate so the driver can shift bonding (e.g., as molecules open/close over
time) without invalidating the finding's id and hash.

`<target>` shapes per variant:

```json
{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}
{"kind":"Contract","id":"spec-work-epic-lifecycle"}
{"kind":"StyleRule","rule_id":"RS-3","subject":"crates/loom-gate/src/integrity.rs"}
{"kind":"Annotation","target_string":"cargo test --lib parse_walks_all_md_files"}
{"kind":"TestPath","path":"crates/loom-gate/src/integrity.rs::test_x"}
{"kind":"LockSite","file":"crates/loom-workflow/src/loop/runner.rs","line":210}
{"kind":"Invariant","spec":"harness","section":"Out of Scope","tag":"loom-runs-podman"}
{"kind":"Template","path":"crates/loom-templates/templates/review.md"}
```

Emit each finding as it is identified, using the common
[Protocol framing](protocol.md#framing). Escaped evidence and Options content
round-trip as ordinary strings; no duplicate prose emission is required.

**Contextual resolution.** After shared decoding, each `RawFinding` resolves its
token, scope, bonds, target variant, and actual referenced content. Unknown
specs, token/target mismatches, or unresolved targets become typed record errors
rather than skipped findings. Mixed failure retains valid findings as well as
errors and terminal context under the pairing contract below. An example or
fenced record that is not a live message cannot satisfy finding enumeration.
Existing bounded recovery handles malformed output; parser agreement alone does
not establish that a finding's diagnosis is semantically correct.

Deterministic verifiers do **not** emit `LOOM_FINDING:` records — they continue
to follow the existing _Verifier-runner contract_ (JSON verdict on stdout, exit
codes). In push and tree act contexts, the driver normalizes each failed
verifier verdict into the same typed Finding record the LLM rubric's lines parse
into, then applies the same routing flow uniformly. The mapping:

| Verifier outcome                                           | `token`                   | `bonds`                          | `target`                       | `evidence`                                                                                                  |
| ---------------------------------------------------------- | ------------------------- | -------------------------------- | ------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| `[check]` / `[test]` / `[system]` exit ≠ 0 (and ≠ 2, ≠ 77) | `verifier-failed`         | `[<spec owning the annotation>]` | `Annotation { target_string }` | verifier's JSON `evidence` field, else stderr tail                                                          |
| Exit code 2 (dispatch error)                               | `dispatch-error`          | same                             | `Annotation { target_string }` | command-not-found / missing-prereq message                                                                  |
| Integrity gate: forward-resolution failure                 | `unresolved-annotation`   | `[<spec owning the annotation>]` | `Annotation { target_string }` | "annotation does not resolve" with spec:line                                                                |
| Integrity gate: stub-pointing                              | `stub-pointing`           | same                             | `Annotation { target_string }` | "annotation points at stub function"                                                                        |
| Integrity gate: atomic-acceptance violation                | `multiple-annotations`    | same                             | `Criterion { spec, anchor }`   | "criterion carries N annotations, expected 1"                                                               |
| Integrity gate: stale pending modifier                     | `unneeded-pending-marker` | same                             | `Annotation { target_string }` | "annotation is now resolved — drop the ? marker" with spec:line                                             |
| Integrity gate: inputs-protocol error                      | `inputs-protocol-error`   | `[<spec owning the annotation>]` | `Annotation { target_string }` | "provider query errored / emitted malformed metadata" with annotation/producer context and actionable cause |

The owning spec for `bonds` is the package containing the dispatched annotation
in its `tests.md`. Shared execution retains each obligation's owner;
[Verify's contract inclusion](verify.md#verifier-inputs-1) accounts for
applicable contracts, criteria, and strategy without inferring ownership from a
file-wide or section-wide input shortcut. Exit code 77 describes skipped
execution, not a pass; unaccepted skips remain blocking verifier evidence. Only
worker capability policy can accept them without claiming coverage. The
`LOOM_FINDING:` wire format is the LLM rubric's emit shape; the typed Finding
record is the in-driver representation both sources converge on.

Review uses [Protocol's admitted terminal variants](protocol.md#phase-admission)
and [Loop's bounded recovery effects](loop.md#verdict-gate). Decision-worthy
review concerns use a clarify-route finding with Inbox's Options brief in
`evidence`; review does not emit direct `Clarify` records or mutate Beads.

`Concern.summary` supplies the verdict-log summary, not per-finding routing.
Malformed concern payloads retain their literal text and valid findings in
`BadWalk::Concern`. The common decoder, not a review-specific JSON pipeline,
defines the accepted payload syntax.

**Streaming + terminator pairing rule.** The walk is a streaming process:
`LOOM_FINDING:` records are emitted as concerns are identified; the terminator
is the final logical message. The driver first cross-checks the raw stream
against the terminator for wire-shape honesty; suppression is applied only after
the shape is well-formed. If the terminator and raw stream disagree, the run
fails with a typed `BadWalk` recovery cause:

| Finding stream         | Terminator                                                      | Verdict                                                                                                                                                                                                                                                                                                                                                |
| ---------------------- | --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 0                      | `LOOM_COMPLETE`                                                 | clean — phase done                                                                                                                                                                                                                                                                                                                                     |
| ≥1 well-formed         | `LOOM_CONCERN: {"summary":"..."}`                               | Apply rubric suppressions to the parsed findings. If ≥1 unsuppressed finding remains: recovery — `RecoveryCause::ReviewConcern { summary, findings: Vec<Finding> }` threaded into `previous_failure` (mint consumes separately). If every parsed finding is suppressed: clean — status output records the suppressed findings and the phase completes. |
| 0                      | `LOOM_CONCERN: {"summary":"..."}`                               | `BadWalk::ConcernWithoutFindings { summary }` — concern claimed without enumeration                                                                                                                                                                                                                                                                    |
| ≥1 well-formed         | `LOOM_COMPLETE`                                                 | `BadWalk::FindingsWithoutConcern { finding_count, findings: Vec<Finding> }` — findings streamed but terminator claims clean; the parsed findings ride through so the next iteration's prompt can name them                                                                                                                                             |
| ≥1 record failed parse | any                                                             | `BadWalk::MalformedFinding { errors, terminal, parsed_findings }` retains every record error, all valid resolved findings, and the independently established or malformed/missing terminal surface                                                                                                                                                     |
| any well-formed (only) | `LOOM_CONCERN:` with malformed JSON / missing / empty `summary` | `BadWalk::Concern { payload, parsed_findings: Vec<Finding> }` — payload parse failure carries the literal malformed text AND any well-formed findings that streamed ahead of the bad terminator                                                                                                                                                        |
| any                    | missing, duplicate, misplaced, or wrong-phase terminal          | Shared protocol/phase error; bounded recovery retains decoded context and raw diagnostics rather than replacing the stream with an empty finding list                                                                                                                                                                                                  |

[Finding failure context](#finding-failure-context) owns the required context
fields and complete production transfer. Templates renders that retained
diagnosis within its prompt budget; bounded presentation does not erase the
underlying typed findings or durable raw evidence.

**Agent's mental model.** Review the diff. Every time you identify a concern,
immediately emit a `LOOM_FINDING:` record with the structured JSON detail and
continue reviewing. When the walk is complete, end your response with
`LOOM_COMPLETE` if you found nothing, or
`LOOM_CONCERN: {"summary": "<one-sentence summary>"}` if you emitted one or more
`LOOM_FINDING:` records. The terminator must match the stream: `LOOM_COMPLETE`
means zero findings, `LOOM_CONCERN` means ≥1 finding.

**Presentation and contract authority.** Protocol owns the Rust message
contract; `partial/findings_walk.md` is the canonical review presentation. Other
templates include it rather than copying its finding/concern emit prose.
[Templates](templates.md#agent-output-conformance) checks actual rendered
examples against shared decoding and contextual finding resolution.

A `[check]`-tier verifier enforces this mechanically: it scans every file under
`crates/loom-templates/templates/` for the literal substrings `LOOM_CONCERN:`
and `LOOM_FINDING:` (the colon-suffixed, wire-format forms — bare-prose mentions
like _"the `LOOM_CONCERN` marker"_ are unaffected) and fails if they appear in
any file other than `partial/findings_walk.md`. Templates that violate this fail
`loom gate check` via the [`check`]-tier dispatcher's non-zero exit code.

##### Structural enforcement

[Acceptance](#verification-surface-matrix--property).

The review-phase classifier consumes a typed, field-private `WalkOutput`, not
raw `&str`. Its required context includes Protocol's original text, spans,
independently decoded messages and shared decoding/admission errors; resolved
findings and finding-resolution errors; and the `TerminalSurface` diagnostic
projection. These categories may live in one sealed context product rather than
separate parallel fields, but none may disappear during adaptation.

`WalkOutput::from_stdout` is the public construction path: it delegates framing
and decoding once to Protocol, resolves independent raw finding candidates, and
retains context even when the session fails. External struct literals, field
mutation, and passing raw text to the classifier fail at compile time.

This seal constrains construction and mutation, not the contents supplied to the
constructor or every downstream transfer. The
[behavioral matrix](#verification-surface) must exercise production capture,
adaptation, classification, and recovery rendering. In particular, valid
findings must reach `GateInputs` and malformed-record failure context rather
than being replaced with default empty vectors. Field presence, privacy, and a
hand-built context fixture alone do not prove that transfer.

##### Resolved finding boundary

[Acceptance](#canonical-contract-location).

`RawFinding` is the deserializable wire/driver DTO. `RawFinding::resolve` checks
nonempty bonds, token/target alignment, scope, target/spec bonding, and
contextual resolution together. Only the resulting `Finding` enters mint/review.
Its fields are private, its accessors are read-only, and it implements
`Serialize` but not `Deserialize`. Editing `into_raw()` output requires
resolution again. Direct struct literals, deserialization into `Finding`, and
field mutation are compile errors.

Finding payload fields and canonical identity are independent of the common
output framing. Consumers deserialize `RawFinding`, then resolve it with their
workspace context; the domain adapter composes that resolution with shared
decoding and preserves the terminal/error/partial-findings matrix.

Deterministic annotation failures resolve against the declared annotation, not
successful command execution: an `unresolved-annotation` finding still names a
real annotation even when its verifier cannot run. Rubric annotation findings
continue to require executable/resolvable targets.

##### Verification surface

[Acceptance](#verification-surface-matrix--property).

The runtime contract is verified at two layers — a behavioral matrix walking
every cell of the failure surface, and a property invariant pinning the typed
Finding's round-trip identity.

**Behavioral matrix (enumerable cells).** A parameterised test walks every cell
of the (stream-shape × terminal-shape) failure surface:

- **Stream-shape axis (4 cells):** zero `LOOM_FINDING:` records; N well-formed
  findings; N well-formed + M malformed (mixed); all- malformed.
- **Terminal-shape axis:** every review-admitted terminal; every other phase's
  terminal as a wrong-phase case; malformed concern/retry/blocked payloads;
  missing, duplicate, earlier, and trailing-text terminal cases.

Each cell checks the typed outcome, every valid finding and raw error retained
through actual classification, and bounded recovery presentation of all context
categories present. Mixed streams include valid records both before and after a
malformed record when their boundaries are independently established. Strict
framing cases also cover fenced examples, marker-looking payload strings,
pretty-printed objects, and unterminated payloads. Independent literal negative
fixtures complement round trips; no historical-log paste-in stands in for
coverage of the class.

**Round-trip property invariant.** For every constructible `Finding` (every
`ConcernToken` × `FindingTarget` canonical combination),
`serde_json::to_string(&finding)` → embed in `LOOM_FINDING:` record → embed in a
synthetic walk output with an arbitrary well-formed terminator →
`parse_walk_output` → assert byte-equal to the input `Finding` and finding id /
hash identical. Extends
`loom-protocol::gate::tests::finding_identity_is_stable_across_runs` from
"identity stable" to "full struct round-trip."

##### Finding id, finding hash, suppression, and dedup

[Acceptance](#findings-and-minting).

Dedup identity is **per finding**, not per batch. Batches are a presentation and
work-queue convenience; changing which sibling findings happen to appear in the
same mint run must not change the identity of any underlying issue.

For each parsed and validated finding the driver computes two values:

- **Finding id** — the canonical, human-readable, versioned semantic identity.
  This is the contract.
- **Finding hash** — a compact hash of the finding id, used for bd labels and
  queries. This is an index key, not the semantic contract.

Finding ids use lower-kebab-case for vocabulary Loom controls and carry an
explicit identity-version prefix:

```text
v1:criterion:verifier-too-narrow:gate#verifier-honesty
v1:invariant:gate#out-of-scope#inline-suppressions
v1:style-rule:rs-3:crates-foo-src-generated-rs
v1:annotation:verifier-bypass:<normalized-target>
```

The id is **target-centred**. Target kinds that already identify a single
concern class omit the token (`style-rule`, `invariant`, `template`,
`test-path`, `lock-site`). Broad target kinds that can host multiple concern
classes include a short lower-kebab concern segment
(`criterion:verifier-too-narrow:...`, `annotation:verifier-bypass:...`). The
canonicalizer is part of `loom-protocol::gate`: a validated `Finding` exposes
its id by combining the `ConcernToken` with the typed `FindingTarget`'s
canonical key. The LLM never emits ids or hashes.

The id deliberately excludes volatile material: evidence text, options prose,
line numbers, batch size, sibling batch membership, current bd parent, and
`bonds` ordering. A multi-spec finding's id follows the target it cites;
cross-spec visibility still comes from `spec:<X>` labels. If a future identity
algorithm changes canonicalization, it bumps the version (`v2`, ...); old labels
remain historical and new runs use the new version explicitly.

The finding hash is persisted on beads as a bd label:

```text
finding:<finding-hash>
```

The hash format is `v<identity-version>:<lowercase-hash>`. The hash algorithm
and length are implementation choices constrained only by bd-label practicality
and collision detection: if two different finding ids produce the same finding
hash in one mint run or against live bd state, mint refuses with a structural
collision instead of merging them.

Before creating or updating remediation, the driver queries bd within the owning
molecule for every bead carrying `finding:<finding-hash>`. The query includes
live workflow states (`open`, `in_progress`, `blocked`, `deferred`) and the
`loom:blocked` / `loom:clarify` / `loom:deferred` labels; closed matches are
fetched separately as same-molecule history.

- **Zero live results, zero closed same-molecule results** — the finding is
  untracked and may enter a new batch.
- **One live result** — update that bead in place or skip minting this finding;
  the run summary names the existing bead id.
- **More than one live result** — structural violation; refuse the run and
  surface the conflicting bead ids.
- **Closed same-molecule result** — treat the finding as already processed for
  this molecule. Record it as reobserved in the summary / deferred-batch
  evidence, but do not create a new bead automatically.

Closed beads outside the owning molecule are history, not suppression: if the
same finding reappears in a later molecule or tree sweep, mint treats it as
actionable current evidence. The summary may mention matching closed beads for
operator context, but outside history cannot mask present drift.

`StyleRule` targets must include a concrete subject in addition to the rule id.
A target of only `rule_id` is too broad: suppressing or deduping `rs-3` globally
would disable the rule rather than track one finding. The subject is the stable
surface the violation applies to (file path plus stable item/anchor when
available, template path, criterion anchor, command surface, or similar
target-specific identifier), normalized by the same lower-kebab canonicalizer
used for the finding id. A line number alone is not a stable subject.

###### Rubric suppression registry

[Acceptance](#findings-and-minting).

Operators can suppress unwanted LLM-rubric noise in the workspace's `loom.toml`
using a top-level TOML array:

```toml
[[suppress]]
id = "v1:criterion:verifier-too-narrow:gate#verifier-honesty"
reason = "False positive: this verifier intentionally checks a broader seam."

[[suppress]]
hash = "v1:abc123def456"
reason = "False positive: generated template intentionally repeats this wording."
```

Exactly one of `id` or `hash` is required. `id` is the canonical finding id and
is preferred when readable; `hash` is the compact finding hash for long
command/path identities. `reason` is required human context and is never parsed
for routing.

Suppression applies only to rubric-origin findings (`LOOM_FINDING:` records
emitted by the LLM walk, including clarify-route tokens such as
`invariant-clash`). It never suppresses deterministic or integrity findings
normalized by the driver, including `verifier-failed`, `dispatch-error`,
`unresolved-annotation`, `stub-pointing`, `unneeded-pending-marker`,
`multiple-annotations`, and `inputs-protocol-error`.

After raw stream / terminator shape validation, suppressed rubric findings are
removed from the gate verdict and from minting. `loom gate review` / `rubric` /
`audit` and `loom gate mint` still report a suppressed-count summary listing
each suppressed finding id, hash, and token so the allowlist stays observable.
If a future rubric emits a changed finding whose id/hash differs, the
suppression no longer matches and the finding resurfaces.

Inline code-comment suppressions are out of scope: comment syntax is
language-specific, some target files have no comments, and many rubric findings
target specs, templates, commands, or seams rather than one source line.

###### Finding status output

[Acceptance](#findings-and-minting).

`LOOM_FINDING:` remains the agent-to-driver wire format. The driver enriches
parsed findings after validation and emits parseable status JSON for
operator/tool output; the LLM does not compute ids, hashes, labels, suppression
decisions, or dedup actions.

A status line is prefixed `LOOM_FINDING_STATUS:` and carries JSON:

```json
{
  "id": "v1:criterion:verifier-too-narrow:gate#verifier-honesty",
  "hash": "v1:abc123def456",
  "label": "finding:v1:abc123def456",
  "token": "verifier-too-narrow",
  "target": {"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"},
  "action": "minted"
}
```

`action` is one of `reported`, `minted`, `skipped-live`, `suppressed`,
`stale-candidate`, `partial-stale-candidate`, or `refused`. Inspection-only
commands (`review`, `rubric`, `audit`) use `reported` for unsuppressed findings;
`mint` uses the lifecycle actions. Human summaries may render the same data as
prose, but the JSON line is the machine-readable surface for suppression
ergonomics and tooling.

###### Stale and partially-stale reporting

[Acceptance](#findings-and-minting).

At `--tree` scope, mint has the whole current finding set for the selected
spec(s), so it reports existing live remediation beads in scope whose
`finding:<hash>` labels no longer align with current unsuppressed findings:

- A live bead whose finding labels have **no** hash in the current set is a
  stale candidate.
- A live bead whose finding labels are a **proper subset mismatch** (some
  current, some absent) is a partially-stale candidate.
- A live bead whose finding labels are all current remains the canonical tracker
  and dedups those findings.

V1 does **not** auto-close or supersede stale candidates. Stochastic rubric runs
can miss or rephrase findings, so closing bd state from a single absence would
be brittle. The report names stale / partial candidate bead ids, current finding
ids, and absent finding ids. Operators or later explicit cleanup commands decide
whether to close or split them. The reporting pass does not run for molecule
promotion or finite inspection scopes because those scopes cannot prove a
missing finding is absent from the whole tree.

##### Deferred remediation processing

[Acceptance](#findings-and-minting).

Molecule-final review and tree sweep produce the same typed Finding records, but
they materialize differently depending on route and scope. `route="blocking"` is
valid at molecule-final review for work that must block the push; at tree scope
the canonical walker emits `deferred` or `clarify`, and a stray `blocking` route
is treated as ready remediation.

1. **Parse** each `LOOM_FINDING:` record into typed fields:
   `{ token, route, bonds, target, evidence }`. Per-record parse errors surface
   as `BadWalk::MalformedFinding` (see _Emit shape_) and the run is refused; the
   recovery cause carries the well-formed remainder so a re-run can fix the
   malformation.

2. **Compute ids / hashes and apply suppressions.** Each validated finding gets
   a finding id and finding hash per _Finding id, finding hash, suppression, and
   dedup_ above. Rubric-origin findings whose id or hash appears in
   `[[suppress]]` are counted in the summary, receive `LOOM_FINDING_STATUS`
   action `suppressed`, and are removed from verdict / materialization.
   Suppression entries matching deterministic or integrity findings are ignored
   and reported as ineffective.

3. **Route per finding.** `blocking` findings at molecule-final review refuse
   the push and create or reuse same-molecule remediation work; at tree scope
   `blocking` is a compatibility alias for ready remediation because there is no
   current push to block. `deferred` findings merge into a molecule-local
   deferred bead at molecule scope, or ready remediation at tree scope.
   `clarify` findings create or update one human-decision bead per finding hash.

4. **Dedup before work-epic allocation.** Query bd by `finding:<finding-hash>`
   across live remediation statuses (`open`, `in_progress`, `blocked`,
   `deferred`) and live beads carrying `loom:clarify`. A live match is updated
   in place or skipped; more than one live match refuses the run as a structural
   violation. At molecule scope, a closed bead in the same molecule counts as a
   seen finding: if the same hash reappears, record it as reobserved on the
   molecule/deferred summary but do not mint another bead automatically. At tree
   scope, if every finding is suppressed, skipped, or deduped to live work, no
   work epic is created and `loom:active` is unchanged.

5. **Validate bonded specs and group by lead.** For each remaining
   deferred/remediation finding, validate that every bonded indexed spec has
   exactly one spec epic (creating and immediately closing a missing
   metadata-only spec epic for an indexed spec when the standing mint path owns
   creation). Pick the lead via _Multi-spec findings_ below — the first element
   of `bonds`. Lead spec determines batch grouping and `spec:<label>` labels
   only; it does not select a parent work epic at tree scope. If a lead spec's
   batch grows too large, split by concern family (`spec-coherence`, `style`,
   `verifier-quality`, etc.), never by individual finding unless it is the only
   finding for that lead.

6. **Store molecule-deferred findings in bd.** Deferred beads are ordinary child
   tasks under the relevant molecule/work epic with `status=deferred`, label
   `loom:deferred`, one `spec:<X>` label per bonded spec, and one
   `finding:<hash>` label per contained finding. No `loom:fixup` label is used;
   a bead carrying `finding:<hash>` labels is already identifiable as
   gate-originated remediation work. bd's `deferred` status keeps the bead out
   of `bd ready` by default.

7. **Admit the decision brief at materialization.** For each remaining
   `route="clarify"` finding, trusted act paths validate evidence and the
   resulting active brief across notes/description under
   [Inbox's unique-brief contract](inbox.md#options-format-contract). Presence
   of one well-formed block cannot hide another active block. Missing, malformed
   or ambiguous briefs route that finding as a single-finding blocked decision
   with cause `clarify-without-options`, instead of applying `loom:clarify`.
   Inspection can diagnose brief defects but neither materializes this fallback
   nor discards an otherwise resolved finding. Existing closed/human-resolved
   history remains governed by scoped dedup, not new active-brief admission.

8. **Promote stabilization work.** `loom gate mint -m/--molecule <id>` updates
   each deferred bead's description with the latest merged evidence, removes
   `loom:deferred`, sets `status=open`, and leaves `finding:<hash>` /
   `spec:<label>` labels intact. If no deferred beads exist, molecule mint is a
   no-op success. Promotion is a state transition, not creation of another bead.

9. **Tree sweep materialization.** `loom gate mint --tree` creates one standing
   remediation work epic for the run only when at least one actionable child
   batch remains after suppression/dedup. It parents every ready fix-up batch,
   blocked-clarify bead, and `loom:clarify` bead from that run under the same
   work epic, applies `loom:active` to that epic, and clears `loom:active` from
   any previous work epic. It does not use `loom:deferred` because the operator
   explicitly requested standing safety-net work. If the driver creates the epic
   but fails before creating any child bead, it closes or otherwise neutralizes
   the empty epic and restores the `loom:active` bookmark to its pre-run state
   before returning failure. If at least one child bead was created, the
   non-empty epic remains open/active and rerun relies on `finding:<hash>` dedup
   to retry only unfinished findings.

End-of-run summary (printed to stdout) lists blocking findings, deferred
findings merged, deferred beads promoted, ready remediation batches created or
updated, clarify findings raised, suppressed rubric findings, stale candidates,
refused structural conflicts, and transient errors. For `--tree` runs that
create an active remediation epic, the summary names the epic id and the
follow-up `loom loop` command. The `LOOM_FINDING_STATUS:` JSON lines carry the
parseable per-finding details.

**Worker discretion on a promoted remediation batch.** The agent dispatched
against a promoted batch reads the description's enumerated findings and decides
whether to fix every finding in one diff, fix a coherent subset and leave the
remaining finding labels on the same molecule's deferred bead, or report framed
human decisions through Protocol's nonterminal `Clarify` records. The separate
terminal reflects the source's actual outcome under
[Loop](loop.md#decision-batches-and-attributed-waits): reporting decisions does
not itself force a source wait or closure. A stabilization bead that produces
new deferred findings merges them back into the same molecule's deferred set
rather than spawning tiny child beads.

##### Multi-spec findings

[Acceptance](#molecule-mint-summary-semantics).

A finding can name more than one spec in `bonds` when the concern spans seams
(e.g., an `orphan-integration` contract spanning two sibling specs). The `bonds`
array is always present, always at least one element; single-spec findings have
a one-element array.

**Lead-spec selection rule (per finding).** The driver validates that each spec
in `bonds` has exactly one spec epic (creating a missing metadata-only spec epic
for an indexed spec when the standing mint path owns the creation, then closing
it immediately). It then picks `bonds[0]` as the lead. This treats the rubric's
ordering as authoritative for primacy while keeping spec epics as metadata
carriers and keeping tree-scope parent selection on the single standing
remediation work epic.

**Batching follows the lead; parenting follows the scope.** A multi-spec finding
joins its lead-spec's batch (per _Deferred remediation processing_ step 5) —
never duplicates across multiple specs' batches. At molecule scope the resulting
batch is parented under that molecule's work epic. At tree scope every resulting
batch is parented under the single standing remediation work epic for the run.
In both scopes the batch bead carries one `spec:<X>` label per unique entry
across the **union of `bonds` over the batch's findings**, so a finding bonded
to {gate, harness} contributes `spec:harness` to a batch that mostly bonds
{gate} alone. Cross-spec searches surface the batch from every named owner's
perspective.

**Bonding shifts are not identity shifts.** The finding id excludes `bonds`
ordering and sibling batch membership. A finding therefore dedups against the
same `finding:<hash>` label even when a new spec joins its `bonds` or the
lead-spec selection changes between runs. Lead-selection is only consulted for
first-mint batch grouping and spec-label assignment, not for identity.

**Validation rule.** For target variants that carry a `spec` field (`Criterion`
and `Invariant`), `target.spec` MUST appear in that finding's `bonds` — the
rubric cannot cite a criterion or invariant in spec X while bonding only to spec
Y. Validation failure rejects the finding with a typed parse error and refuses
the mint run (per _Deferred remediation processing_ step 1).

#### Output

[Acceptance](#findings-and-minting).

The gate's output is a verdict (pass / hard-fail / clarify) plus any flagged
actions. Gate invocations also write JSONL evidence logs under
`.loom/logs/gate/`; `bd` issues and git commits remain the durable work record.

- **Worker/per-bead deterministic failures** drive the existing recovery loop
  with `previous_failure` context. They do not produce Finding records or
  remediation batches.
- **Push-range rubric findings** route by their explicit `route` field:
  `blocking` findings refuse the push and create or reuse same-molecule
  remediation work, `deferred` findings merge into molecule-local
  `loom:deferred` beads, and `clarify` findings materialize one human- decision
  bead per finding hash. Suppressed rubric findings are reported in summaries
  but do not affect verdicts or bd state.
- **Tree-scope deterministic + unsuppressed rubric findings** (`mint --tree`)
  materialize as ready remediation batches under one standing remediation work
  epic for the run — grouped by lead-spec / concern family after per-finding
  dedup, but not parented under per-spec work epics. If no actionable child
  batch remains, no work epic is created.
- **Push-gate integrity findings** (per _Integrity gate_'s recovery branch)
  merge into the molecule's deferred remediation set and are promoted by
  `loom gate mint -m/--molecule <id>` during stabilization.
- **Clarify-route findings** (currently defaulted only by `invariant-clash`;
  future default-clarify tokens follow the same path automatically) mint as
  single-finding beads — one bead per finding, never bundled — carrying
  `loom:clarify` with the `## Options — …` block from the finding's `evidence`
  rendered into the bead's description per the Options Format Contract. The
  per-finding shape is load-bearing because `loom inbox` cannot consume a bead
  carrying multiple active options blocks. Trusted materialization applies
  Inbox's unique-brief rule; missing, malformed or ambiguous briefs fall back to
  `loom:blocked` with cause `clarify-without-options` on that decision rather
  than minting a stranded clarify bead. Inspection preserves the finding and
  diagnostic without materializing either queue state.

Past gate runs are persisted for observability, but _past passes don't grant
immunity from re-evaluation_. Conformance is a property of the current code-spec
pair, tree, config, and push range, not a historical fact.

### Functional

1. **Options-block requirement on clarify-bound findings.**
   `partial/findings_walk.md` requires every clarify-bound finding, not only
   `invariant-clash`, to embed one canonical brief inside its `evidence` payload
   under [Inbox — Options Format Contract](inbox.md#options-format-contract).
   Trusted Findings materialization validates evidence and the resulting unique
   active brief before human-queue admission. Missing, malformed or ambiguous
   briefs use the single-decision `loom:blocked` fallback with cause
   `clarify-without-options`; inspection diagnoses them without Beads writes or
   loss of resolved finding context. The brief remains evidence content, not a
   separate wire payload or decoding authority. The reviewer should emit a typed
   `Blocked` terminal when it cannot safely articulate options, rather than emit
   a clarify-bound finding without them.

## Out of Scope

- Semantic review policy belongs to Gate, human decisions to Inbox, and workflow
  scheduling to Loop. Resolved findings are not freely mutable wire DTOs.

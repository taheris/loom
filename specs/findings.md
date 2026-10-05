# Findings and remediation

Resolves untrusted finding records into immutable findings and materializes
scoped, bonded, deduplicated remediation.

## Problem Statement

Resolves untrusted finding records into immutable findings and materializes
scoped, bonded, deduplicated remediation. This package is one contract owner,
not a new crate or command tree.

## Architecture

Inputs, outputs, and trust boundaries are stated in the contracts below. Related
owners: [gate](gate.md), [verify](verify.md), [specs](specs.md),
[inbox](inbox.md), [loop](loop.md), [harness](harness.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Finding failure context

[Acceptance](#wire-format-strict-validation-and-max-context-preservation).

```rust
pub enum BadWalk {
    /// `LOOM_CONCERN:` payload did not parse as
    /// `{"summary": "<non-empty>"}` — invalid JSON, missing
    /// `summary` field, or empty `summary`. The literal post-marker
    /// text is preserved for the recovery prompt, AND any
    /// well-formed `LOOM_FINDING:` records that streamed ahead of
    /// the bad terminator are preserved in `parsed_findings` so the
    /// agent's diagnosis is not lost when only the terminal was
    /// malformed.
    Concern { payload: String, parsed_findings: Vec<Finding> },

    /// Terminator claimed concern but zero `LOOM_FINDING:` records
    /// streamed during the walk. The parsed summary is preserved
    /// so the recovery prompt can quote it back.
    ConcernWithoutFindings { summary: String },

    /// One or more `LOOM_FINDING:` records streamed but the
    /// terminator was `LOOM_COMPLETE`. The count AND the parsed
    /// findings are preserved so the next iteration's prompt can
    /// name them, and so `loom gate mint` can consume the same
    /// records on the next walk rather than re-deriving them.
    FindingsWithoutConcern { finding_count: usize, findings: Vec<Finding> },

    /// One or more `LOOM_FINDING:` records failed parse (most
    /// common: trailing backticks from markdown fencing on an
    /// otherwise-valid JSON payload). `errors` is one
    /// `FindingParseError` per malformed record. `terminal` is the
    /// well-formed terminator (or its typed
    /// `Missing`/`Malformed` placeholder) so the agent's next
    /// iteration sees BOTH the per-record malformation detail AND
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
    Waiting,
    Concern { summary: String },
    Retry { reason: String },
    Blocked { reason: String },
    Clarify { question: String },
    Malformed { payload: String },
    Missing,
}
```

`FindingParseError` is re-exported from `loom-workflow::review::finding` (per
[Findings — Findings and Minting](#findings-and-minting-1)) — the typed
wire-format error the parser produces. Carrying a `Vec<FindingParseError>` in
`BadWalk::MalformedFinding` means each per-record malformation rides through
with its starting `line_number`, the literal `raw` record text, and the typed
reason (`Json`, `UnknownToken`, `TokenVariantMismatch`, `UnknownBondSpec`,
`UnresolvedTarget`, `TargetSpecNotInBonds`).

**Maximum-context preservation invariant.** Every `BadWalk` variant carries the
maximum well-formed context by struct shape; construction without the parseable
pieces is a compile error. The "lost the agent's diagnosis when one piece of the
walk was malformed" failure mode is structurally unrepresentable. See
[Findings — Streaming + terminator pairing rule](#findings-and-minting-1) for
the cross-product of (stream-shape × terminal-shape) cells the variants cover.

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

### Relocation and attribution

[Acceptance](#relocation-and-attribution-1).

Cross-label moves change label/anchor-dependent finding identities. Existing
finding, suppression, dedup, and status references require explicit
reconciliation, not automatic attachment by text similarity. Suppression must
not silently cover a different claim; known claim-attached negative evidence
retains linkage under Evidence. This transition does not introduce a permanent
legacy alias registry.

## Success Criteria

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

- Long evidence may include raw line breaks inside JSON strings; the driver
  normalizes them before typed validation and preserves the resulting evidence
  line breaks
  [test](raw_multiline_evidence_is_normalized_before_strict_validation)

- The review walk terminates with exactly one of `LOOM_COMPLETE`,
  `LOOM_CONCERN: {"summary": "..."}`, `LOOM_RETRY`, or `LOOM_BLOCKED`;
  `LOOM_BLOCKED` includes a reason explaining why no options can be safely
  surfaced, review clarifications are `route="clarify"` findings with Options in
  `evidence`, and a walk that emits `LOOM_FINDING:` records without a terminal
  marker fails the mint invocation with non-zero exit
  [test](mint_walk_without_terminal_marker_fails_run)

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
  `RecoveryCause::BadWalk(BadWalk:: FindingsWithoutConcern { finding_count })`
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

- A malformed `LOOM_FINDING:` record — invalid JSON after raw string line-break
  normalization, unknown token, unknown spec, target variant mismatching token,
  or unresolved target content — fails the mint invocation with a typed parse
  error naming the offending record's start line; no silent skip
  [test](mint_malformed_loom_finding_fails_run_with_typed_error)

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
  `clarify-without-options` — never a stranded clarify bead the chat-drafter
  cannot resolve
  [test](mint_clarify_bound_finding_without_options_falls_back_to_blocked)

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
  for deferred work, or emit `LOOM_CLARIFY` for no-progress cases; the bead's
  acceptance criterion is "agent processed the batch", not "every finding
  individually resolved"
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

- A `LOOM_FINDING:` record whose JSON payload fails parse after raw string
  line-break normalization — invalid JSON (most common: trailing backticks from
  markdown fencing), unknown `token`, target/token variant mismatch, unresolved
  spec label or anchor — surfaces as
  `RecoveryCause::BadWalk(BadWalk::MalformedFinding { errors, terminal })` with
  the well-formed terminal preserved alongside the per-record parse errors
  [test](backtick_wrapped_loom_finding_line_routes_to_bad_walk_malformed_finding_with_terminal_preserved)

- The `LOOM_FINDING:` substring match is case-sensitive and colon-suffixed;
  bare-prose mentions without the colon do not match
  [test](loom_finding_substring_match_requires_uppercase_and_colon_suffix)

- `BadWalk::Concern` carries `{ payload, parsed_findings: Vec<Finding> }`;
  well-formed findings streamed ahead of a malformed terminal are preserved in
  `parsed_findings`
  [test](bad_walk_concern_preserves_well_formed_findings_alongside_malformed_payload)

- `BadWalk::FindingsWithoutConcern` carries
  `{ finding_count, findings: Vec<Finding> }`; the parsed findings ride through
  so the next iteration's prompt and `loom gate mint` can both consume them
  [test](bad_walk_findings_without_concern_carries_parsed_findings_vec)

- The `BadWalk::MalformedFinding { errors, terminal }` variant carries every
  per-record parse error AND the well-formed terminal surface (or a typed
  `Missing`/`Malformed` variant when the terminal itself failed). Construction
  without both pieces is a compile error
  [test](bad_walk_malformed_finding_variant_carries_errors_and_terminal_by_struct_shape)

### Verification surface (matrix + property)

- The review-phase classifier signature consumes a typed `WalkOutput` (with
  field-private struct + `pub WalkOutput::from_stdout` constructor that runs
  `parse_walk_output` internally), not raw `&str`. Any production caller passing
  a `&str` is a compile error, and any caller constructing `WalkOutput` with
  bogus fields cannot compile because the fields are private at the
  `loom-protocol` crate boundary
  [test](classify_review_phase_signature_requires_typed_walk_output)

- The (stream-shape × terminal-shape) failure matrix is exhaustive: every cell
  in the 4 × 7 cross-product (S0..S3 stream shapes × seven terminal shapes,
  including wrong-phase `LOOM_WAITING`) has a parameterised test asserting the
  typed outcome variant and the maximum-context invariant
  [test](walk_output_failure_matrix_routes_every_cell_with_typed_outcome_and_preserves_max_context)

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

- The loop-only bare `LOOM_WAITING` marker parses as `ExitSignal::Waiting`; it
  carries no payload because workflow validation resolves its blocker evidence
  from Beads [test](waiting_marker_parses_as_typed_exit_signal)

- `loom-workflow::review::finding` (the `WalkOutput` typed product +
  `parse_walk_output` parser) and `loom-workflow::todo::exit::ExitSignal` /
  `parse_exit_signal` move to `loom-protocol::gate`. Existing `loom-workflow`
  imports either remap or re-export
  [test](loom_workflow_re_exports_walk_output_and_exit_signal_from_loom_protocol)

- The `WalkOutput` struct's fields are private; `WalkOutput::from_stdout` is
  `pub` (consumers need to call it) but is the only construction path. The
  silent-loss failure class — production caller constructs `WalkOutput` with
  bogus fields, bypassing the typed parse pipeline — is structurally
  unrepresentable via field-privacy, not via `pub(crate)` constructor scoping
  [test](walk_output_fields_private_only_constructor_is_from_stdout)

<!-- prettier-ignore -->
- The `finding_no_duplicate_definitions` walker continues to enforce one
  canonical definition of `Finding`, `ConcernToken`, `FindingTarget`,
  `WalkOutput`, `BadWalk`, and `ExitSignal` across the workspace; the canonical
  home after extraction is `loom-protocol::gate` [check](cargo run -p loom-walk
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
  [Loop — Verdict Gate](loop.md#verdict-gate). This spec receives the resulting
  deterministic gate evidence and owns its trust semantics.

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
[Harness](harness.md#functional).

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

- `BadWalk` enum carries
  `Concern { payload: String, parsed_findings: Vec<Finding> }`,
  `ConcernWithoutFindings { summary: String }`,
  `FindingsWithoutConcern { finding_count: usize, findings: Vec<Finding> }`, and
  `MalformedFinding { errors: Vec<FindingParseError>, terminal: TerminalSurface }`;
  the wrapped pattern preserves parsed context just as
  `RecoveryCause::ReviewConcern { summary, findings }` does
  [test](bad_walk_variants_preserve_max_context_invariant_by_struct_shape)

- Maximum-context preservation invariant: `BadWalk::Concern` carries
  `parsed_findings` (any well-formed findings streamed ahead of the malformed
  terminator); `BadWalk::FindingsWithoutConcern` carries `findings` (the parsed
  Vec<Finding> the agent emitted); and `BadWalk::MalformedFinding` carries the
  well-formed `terminal` alongside the per-record errors. Construction of any
  variant without its max-context fields is a compile error
  [test](bad_walk_variants_preserve_max_context_invariant_by_struct_shape)

- `TerminalSurface` enum mirrors `ExitSignal`, including loop-only `Waiting`,
  with explicit `Malformed { payload: String }` and `Missing` variants so
  `BadWalk::MalformedFinding`'s `terminal` field can carry the terminal state
  regardless of whether the terminal itself parsed
  [test](terminal_surface_carries_malformed_and_missing_variants)

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

`loom gate mint` is the gate's sole driver-side mint surface — the one command
that walks the rubric and produces remediation beads. Every other gate
subcommand is inspection-only (no bd writes). Mint is what makes the rubric's
concerns actionable.

##### Canonical contract location

[Acceptance](#canonical-contract-location).

The Rust contract for the gate's wire format is owned by `loom-protocol::gate` —
a leaf crate carrying the `Finding` record struct + `ConcernToken` closed enum +
`FindingTarget` internally-tagged-on-`kind` enum + `TargetKind` +
`FindingValidator` trait + `FindingParseError` + `BadWalk` + `TerminalSurface` +
`WalkOutput` + `WalkOutputError` + `ExitSignal` + the `parse_walk_output` /
`WalkOutput::from_stdout` / `parse_exit_signal` parsers + the
`LOOM_FINDING_PREFIX` constant.

**`pub` / `pub(crate)` boundary.** The public surface is the typed contract a
consumer needs to construct, match on, or read from a parsed walk: `Finding`,
`ConcernToken`, `FindingTarget`, `TargetKind`, `FindingValidator`,
`FindingParseError`, `BadWalk`, `TerminalSurface`, `WalkOutput`,
`WalkOutputError`, `ExitSignal`, `LOOM_FINDING_PREFIX`, `parse_walk_output`,
`WalkOutput::from_stdout`, `parse_exit_signal`, and `Finding::id` /
`Finding::hash`. The following stay `pub(crate)` so the implementation can
reshape without a major bump: checks internal to `RawFinding::resolve`,
per-variant `canonical_form` identity helpers, raw-payload parsing helpers
(single-line parser — consumers go through `parse_walk_output` for the full
pipeline), and internal helpers like `terminal_surface_from_stdout`. Widening
later is cheap; narrowing is a breaking change.

**The seal is field-private, not constructor-private.** The silent-loss failure
class — production caller constructs `WalkOutput` with bogus fields and the
typed terminal/finding pipeline is bypassed — is structurally unrepresentable
because `WalkOutput`'s fields are private at the `loom-protocol` crate boundary.
`WalkOutput::from_stdout` is `pub` so consumers can call it, and it's the only
construction path. `Finding` also has private fields and borrowed read-only
accessors. Untrusted input constructs `RawFinding`; resolution produces the
immutable `Finding` consumed by review and mint. Editing its raw projection
requires resolution again; public mutable fields cannot bypass that boundary.
The [resolved finding boundary](#resolved-finding-boundary) governs
construction. Crate dependencies and wire versioning follow
[Harness](harness.md#canonical-contract-location).

**Cross-repo consumers.** External consumers (e.g. wrix) depend on
`loom-protocol` directly. The expected consumption shape is: spawn
`loom gate review` / `loom gate mint` as a subprocess, capture stdout, call
`loom-protocol::gate::parse_walk_output(&stdout, &validator)`. The typed
`WalkOutput` is the consumer's entry point into the parsed walk. Compile-time
type safety + the leaf-crate dependency shape gives consumers the same
guarantees loom's own internal pipeline has.

The contract types previously defined at `loom-templates::finding` relocate to
`loom-protocol::gate` in a single atomic migration diff. The
`finding_no_duplicate_definitions` walker continues to enforce the
single-definition property across the workspace.

The wire format's sole textual definition for _agent-facing prose_ lives at
`crates/loom-templates/templates/partial/findings_walk.md`; the anti-drift
`[check]`-tier verifier (defined in _Emit shape → Single source of truth_ below)
refuses any template that restates the `LOOM_FINDING:` / `LOOM_CONCERN:`
colon-suffixed forms outside that partial. The partial documents the wire format
for LLM agents; `loom-protocol` documents it for Rust consumers. They are pinned
to the same loom release via Cargo + the workspace's git ref; the existing
anti-drift walker covers both surfaces.

**`ConcernToken` is not `ReviewConcern`.** Two enums look similar and live in
different crates with different purposes. `ConcernToken` (in
`loom-protocol::gate`) is the **wire-level identifier** on each streamed
`LOOM_FINDING:` record — the closed set of tokens (`spec-coherence-fail`,
`orphan-integration`, `verifier-bypass`, …) the rubric emits and
`loom gate mint` routes on. `ReviewConcern` (in
`loom-workflow::review::phase_verdict`) is a separate 12-variant enum that
previously named the terminal `LOOM_CONCERN` token; under the retired
terminal-token contract (per the review rubric's _Streaming + terminator pairing
rule_), the terminal carries only `{"summary": "..."}` and per-finding routing
is decided on each `LOOM_FINDING:` record's `ConcernToken`, not on the terminal.
`ReviewConcern` survives as a **display vocabulary** for `bd update --notes` and
verdict-log human-readable cause labels (derived from `findings[0].token` or a
"multiple" label when heterogeneous); it has no routing role.

##### Inspection vs. act partition

[Acceptance](#findings-and-minting).

Every gate subcommand except `loom gate mint` is **inspection-only** — it walks
rules and emits findings to stdout but performs no `bd` writes. `mint` is the
sole bd-mutation chokepoint. The partition is structural, not advisory: no code
path inside `loom gate audit` / `verify` / `review` / `judge` / `rubric` /
`check` / `test` / `system` / `verify-marker` may call into the mint pipeline's
`bd` write surface as a side-effect. A `[check?]`-tier verifier asserts this
(deferred to land alongside the broad forward-resolution change under
[_Pending modifier_](verify.md#pending-modifier) below) by scanning production
sources for `mint_findings` / `mint_finding_with_options` invocations outside
`loom-workflow::mint` and outside `loom loop`'s verdict-gate routing path.

The driver's `loom loop` per-bead path is an **operator-level composition**
around the gate, not a side-effect of any inspection subcommand: after
integration it runs deterministic `verify --diff <pre-integration-head>..HEAD`
and records a typed gate log. The molecule-completion push gate deliberately
composes pre-push deterministic verification with
`review --diff <actual-push-range>` without invoking `mint`; findings ride
through the review-log file and through the typed recovery/remediation surfaces.
Stabilization or an explicit `loom gate mint` invocation is what consumes
findings as bd state.

The `MarkerProof` mint at the molecule-completion push gate (see `## Marker`
below) is a **separate** mint surface, owned by `loom-gate::marker` with its own
`pub(crate)` constructor — it writes a single content-addressed JSON file to
`.loom/marker.json`, never bd state. "Audit makes no bd writes" remains true
through that path; the marker is filesystem state, not bd state.

##### Wire-format mixed-shape principle

[Acceptance](#review-finding-wire-contract).

The wire format the rubric walk emits is shaped by one principle that governs
every marker the driver consumes: **JSON-payload for markers the driver routes
on, bare for markers whose context comes from adjacent prose or durable workflow
state.** `LOOM_FINDING:` and `LOOM_CONCERN:` carry JSON because the driver
routes per-finding tokens and the terminal summary needs structured framing.
`LOOM_COMPLETE` / `LOOM_NOOP` / `LOOM_WAITING` / `LOOM_RETRY` / `LOOM_BLOCKED` /
`LOOM_CLARIFY` are bare. `LOOM_WAITING` reads its durable state from the Beads
dependency graph; the other bare markers read context (reason / question) from
the prior non-empty line; LLM agents narrate the reason in prose and emit the
marker as a yes/no terminator without having to compose a JSON object in the
same turn. Mixing in either direction — JSON payload for a bare marker, bare
payload for a routed marker — is a wire-format violation and is rejected by the
typed parser (`loom-workflow::todo::exit::parse_exit_signal` for terminals;
`loom-workflow::review::finding::parse_walk_output` for the streaming finding
lines).

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

The target variant is architecture-bearing — its shape is what makes "every
finding carries a target appropriate to its token" structurally unrepresentable
as a mismatch. See
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
  evidence MUST embed the canonical `## Options — …` block per the _Options
  Format Contract_. Gate routing validates this at parse time and falls back to
  `loom:blocked` with cause `clarify-without-options` when the options block is
  absent — see _Deferred remediation processing_ below.

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

Emit compact one-line JSON where possible, as the finding is identified (not
batched at end-of-walk). Long evidence and `route="clarify"` Options blocks are
allowed to span lines inside JSON string values: before `serde_json`
deserialization, the driver normalizes raw line breaks that appear inside a JSON
string to `\n` escapes, then applies the same typed validation. Newlines outside
strings are accepted only as ordinary JSON whitespace within the same object.
JSON was chosen over pipe-delimited specifically because LLM emit is more
reliable on JSON than on bespoke formats — the target's tagged-union shape
encodes naturally, escaping is well-known, and field-order independence avoids
one class of malformed emit.

**Strict parse-time validation.** The `LOOM_FINDING:` prefix is matched by
substring search in the agent's stdout, so backtick-wrapped, markdown-fenced, or
prose-prefixed records are still detected. The match is case-sensitive on the
literal string `LOOM_FINDING:` (with the trailing colon); bare-prose mentions
without the colon (e.g. _"the `LOOM_FINDING` marker"_) do not match by design.

A record that matches the substring but fails the strict validation that follows
— malformed JSON after raw string line-break normalization (most common:
trailing backticks from markdown fencing), unknown `token`, any element of
`bonds` that doesn't resolve to a workspace spec label, `target` variant
mismatching `token`'s expected variant, or unresolved target content (criterion
anchor not in spec, file path absent on disk) — surfaces as
`BadWalk::MalformedFinding { errors, terminal }` per the pairing-rule table
below, with the well-formed terminal preserved alongside the per-record errors.
**No silent skip.** The substring-then-strict- validate shape catches
accidentally-fenced finding emit while loudly typing the malformation, which is
what makes the wire format observable rather than fragile.

The walk is assumed to be retry-friendly: a re-run typically gets the shape
right; a persistently-malformed emit is signal that the prompt or rubric needs
adjusting.

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

The walk terminates with exactly one terminator on the final non-empty line (per
[Loop — Verdict Gate](loop.md#verdict-gate)): `LOOM_COMPLETE`, `LOOM_CONCERN`,
`LOOM_RETRY`, or `LOOM_BLOCKED`. `LOOM_RETRY` indicates the walk could not
complete for environmental reasons (logs corrupt, workspace inaccessible,
transient IO) and a fresh dispatch should retry the walk — preferred over
`LOOM_BLOCKED` for the "I couldn't review" failure mode unless the reviewer also
has no candidate resolution to enumerate. `LOOM_BLOCKED` means the walk could
not complete, the reviewer has no candidate resolution to surface, and the
reason explains why options cannot be safely enumerated. Direct `LOOM_CLARIFY`
is not a review terminator: a reviewer that can enumerate options emits a
`route="clarify"` finding with the Options block in `evidence` and terminates
with `LOOM_CONCERN`. `LOOM_COMPLETE` and `LOOM_CONCERN` are the verdict-carrying
terminators and are governed by the pairing rule below.

**`LOOM_CONCERN` payload — JSON shape and parse discipline.** The payload is a
JSON object with a single required field, `summary`, whose value is a non-empty
string: `LOOM_CONCERN: {"summary": "<one-sentence summary>"}`. The driver parses
the payload with the same `serde_json` pipeline that consumes `LOOM_FINDING:`
records. Parse failures — invalid JSON, missing `summary`, empty `summary` —
surface as the typed `BadWalk::Concern { payload, parsed_findings }` recovery
cause (defined in [Loop](loop.md#verdict-gate)) so the recovery prompt can carry
the literal text that failed and the agent can fix the shape on the next
iteration. The summary is for the verdict log only; the actionable detail lives
in the streamed `LOOM_FINDING:` records, and per-finding routing is decided by
`loom gate mint` on each finding's token, not on the terminal marker. The
terminal token-and-reason form (`<token> -- <reason>`) is retired; the terminal
token only ever duplicated the strongest finding's token at the cost of
structural complexity.

**Streaming + terminator pairing rule.** The walk is a streaming process:
`LOOM_FINDING:` records are emitted as concerns are identified; the terminator
is the final line. The driver first cross-checks the raw stream against the
terminator for wire-shape honesty; suppression is applied only after the shape
is well-formed. If the terminator and raw stream disagree, the run fails with a
typed `BadWalk` recovery cause:

| Finding stream         | Terminator                                                      | Verdict                                                                                                                                                                                                                                                                                                                                                                           |
| ---------------------- | --------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0                      | `LOOM_COMPLETE`                                                 | clean — phase done                                                                                                                                                                                                                                                                                                                                                                |
| ≥1 well-formed         | `LOOM_CONCERN: {"summary":"..."}`                               | Apply rubric suppressions to the parsed findings. If ≥1 unsuppressed finding remains: recovery — `RecoveryCause::ReviewConcern { summary, findings: Vec<Finding> }` threaded into `previous_failure` (mint consumes separately). If every parsed finding is suppressed: clean — status output records the suppressed findings and the phase completes.                            |
| 0                      | `LOOM_CONCERN: {"summary":"..."}`                               | `BadWalk::ConcernWithoutFindings { summary }` — concern claimed without enumeration                                                                                                                                                                                                                                                                                               |
| ≥1 well-formed         | `LOOM_COMPLETE`                                                 | `BadWalk::FindingsWithoutConcern { finding_count, findings: Vec<Finding> }` — findings streamed but terminator claims clean; the parsed findings ride through so the next iteration's prompt can name them                                                                                                                                                                        |
| ≥1 record failed parse | any                                                             | `BadWalk::MalformedFinding { errors: Vec<FindingParseError>, terminal: TerminalSurface }` — per-record errors are preserved alongside the typed terminal surface (well-formed terminal kept as-is; when the terminator also fails parse, the terminal is carried via `TerminalSurface::Malformed { payload }` so both failure pieces ride through the `MalformedFinding` variant) |
| any well-formed (only) | `LOOM_CONCERN:` with malformed JSON / missing / empty `summary` | `BadWalk::Concern { payload, parsed_findings: Vec<Finding> }` — payload parse failure carries the literal malformed text AND any well-formed findings that streamed ahead of the bad terminator                                                                                                                                                                                   |
| any                    | missing or duplicate marker                                     | `SwallowedMarker` (existing)                                                                                                                                                                                                                                                                                                                                                      |

**Maximum-context preservation invariant.** Every `BadWalk` variant carries the
maximum well-formed context by struct shape. Failure mode "lost the agent's
diagnosis when one piece of the walk was malformed" is structurally
unrepresentable — the type cannot be constructed without the parseable pieces
(well-formed findings preserved alongside a malformed terminal; well-formed
terminal preserved alongside malformed findings). This is what `templates.md`'s
`PreviousFailure::BadWalk(BadWalk)` rendering relies on to produce a useful
recovery prompt regardless of which piece failed parsing.

**Agent's mental model.** Review the diff. Every time you identify a concern,
immediately emit a `LOOM_FINDING:` record with the structured JSON detail and
continue reviewing. When the walk is complete, end your response with
`LOOM_COMPLETE` if you found nothing, or
`LOOM_CONCERN: {"summary": "<one-sentence summary>"}` if you emitted one or more
`LOOM_FINDING:` records. The terminator must match the stream: `LOOM_COMPLETE`
means zero findings, `LOOM_CONCERN` means ≥1 finding.

**Single source of truth.** The wire-format definitions for both `LOOM_FINDING:`
and `LOOM_CONCERN:` live exactly once, in
`crates/loom-templates/templates/partial/findings_walk.md`. Other templates that
need to talk about these markers `{% include %}` that partial; they never
restate the format. The bare-marker partials (`partial/progress_markers.md` for
`LOOM_COMPLETE` / `LOOM_NOOP`, `partial/self_report_markers.md` for loop/todo
`LOOM_RETRY` / `LOOM_BLOCKED` / `LOOM_CLARIFY`, and
`partial/review_self_report_markers.md` for review cannot-complete self-reports)
describe bare-marker semantics without redefining the review-walk markers.

A `[check]`-tier verifier enforces this mechanically: it scans every file under
`crates/loom-templates/templates/` for the literal substrings `LOOM_CONCERN:`
and `LOOM_FINDING:` (the colon-suffixed, wire-format forms — bare-prose mentions
like _"the `LOOM_CONCERN` marker"_ are unaffected) and fails if they appear in
any file other than `partial/findings_walk.md`. Templates that violate this fail
`loom gate check` via the [`check`]-tier dispatcher's non-zero exit code.

##### Structural enforcement

[Acceptance](#production-walker-wiring).

The review-phase classifier signature (`classify_review_phase` in
`loom-workflow::review::production`) consumes a typed `WalkOutput` product
(`{ terminal: TerminalSurface, findings: Vec<Finding>, finding_errors: Vec<FindingParseError> }`),
not raw `&str`. `WalkOutput::from_stdout` is the only construction path: it
takes the agent's combined stdout and a `FindingValidator`, runs the parse
pipeline once, and returns the typed product. The classifier cannot be called
with raw `&str`; that becomes a compile error. The silent-loss failure class — a
production caller constructs `GateInputs` without invoking the walk parser,
leaving the typed finding stream at default empty so every well-formed
`LOOM_CONCERN` with streamed findings collapses to
`BadWalk::ConcernWithoutFindings` — becomes structurally unrepresentable.

The seal is **field-private**: `WalkOutput`'s fields are private at the crate
boundary, and `WalkOutput::from_stdout` is `pub` (consumers depending on
`loom-protocol` need to call it). Field privacy is what makes the silent-loss
class unrepresentable — struct-literal construction with bogus fields cannot
compile, so any `WalkOutput` reaching the classifier ran the typed parse
pipeline. This mirrors the sealed-`MarkerProof` pattern (`## Marker` below):
validated construction through a single entry point is the type-shape contract
for trust handoff.

##### Resolved finding boundary

[Acceptance](#canonical-contract-location).

`RawFinding` is the deserializable wire/driver DTO. `RawFinding::resolve` checks
nonempty bonds, token/target alignment, scope, target/spec bonding, and
contextual resolution together. Only the resulting `Finding` enters mint/review.
Its fields are private, its accessors are read-only, and it implements
`Serialize` but not `Deserialize`. Editing `into_raw()` output requires
resolution again. Direct struct literals, deserialization into `Finding`, and
field mutation are compile errors.

This intentionally changes the Rust construction API, not the JSON wire shape or
identity/hash algorithm: consumers deserialize `RawFinding`, then resolve it
with their workspace context. The streaming parser does this internally and
preserves its terminal/error/partial-findings matrix.

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
- **Terminal-shape axis (7 cells):** `LOOM_COMPLETE`; `LOOM_NOOP`; wrong-phase
  `LOOM_WAITING`; `LOOM_CONCERN:` with valid JSON; `LOOM_CONCERN:` with the
  legacy `<token> -- <reason>` shape; `LOOM_CONCERN:` with malformed JSON
  (missing field, empty `summary`, invalid JSON); no terminal on the final
  non-empty line.

28 cells. Each cell asserts (a) the typed outcome variant, (b) the
maximum-context preservation invariant (every parseable piece of the input
appears in the outcome), (c) the `Display for PreviousFailure` rendering is
non-empty and references both pieces when both are present.

No historical-log paste-ins; the matrix covers the general class. The one
existing one-shot replay test
(`legacy_token_reason_payload_routes_to_bad_walk_concern` from lm-448x.4) stays
as one regression test for the legacy `<token> -- <reason>` form's `BadWalk`
routing but is not the load-bearing class coverage.

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

7. **Validate clarify coupling.** For each `route="clarify"` finding, scan
   `evidence` for the canonical `## Options — <summary>` heading followed by at
   least one `### Option <N> — <title>` subsection. If absent or malformed,
   route that finding as a single-finding blocked bead with cause
   `clarify-without-options` instead of applying `loom:clarify`.

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
remaining finding labels on the same molecule's deferred bead, or emit
`LOOM_CLARIFY` if no progress is possible. A stabilization bead that produces
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
  carrying multiple options blocks. Clarify-route findings whose evidence lacks
  a well-formed options block fall back to `loom:blocked` with cause
  `clarify-without-options` rather than minting a stranded clarify bead.

Past gate runs are persisted for observability, but _past passes don't grant
immunity from re-evaluation_. Conformance is a property of the current code-spec
pair, tree, config, and push range, not a historical fact.

### Functional

20. **Options-block requirement on clarify-bound findings.**
    `partial/findings_walk.md` requires every clarify-bound finding (any token
    whose mint would label the resulting bead `loom:clarify`, not only
    `invariant-clash`) to embed the canonical `## Options — <summary>` block
    (with at least one `### Option <N> — <title>` subsection) inside its
    `evidence` payload. The driver-side `loom gate mint` validates the evidence
    at parse time; clarify-bound findings whose evidence lacks a well-formed
    options block fall back to `loom:blocked` with cause
    `clarify-without-options` per
    [Inbox — Options Format Contract](inbox.md#options-format-contract). No
    wire-format extension to the `LOOM_FINDING:` JSON payload — the contract
    lives in the `evidence` field's content, with the enforcement at the mint
    chokepoint. The agent should emit `LOOM_BLOCKED` directly when it cannot
    articulate options, with a reason explaining why no options can be safely
    surfaced, rather than emitting a clarify-bound finding without them.

## Out of Scope

- Semantic review policy belongs to Gate, human decisions to Inbox, and workflow
  scheduling to Loop. Resolved findings are not freely mutable wire DTOs.

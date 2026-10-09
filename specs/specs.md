# Spec packages and identity

Discovers canonical spec packages, parses acceptance, and resolves stable
criterion references against current snapshots.

## Problem Statement

Discovery, task assignment, and evidence attribution must agree on which
requirements exist and how they are identified. Package relocation must not hide
acceptance, and changed requirements must not silently inherit old authority.

## Architecture

Package discovery and structural acceptance parsing produce identities and
snapshot-resolved references for workflow consumers. Related owners:
[todo](todo.md), [loop](loop.md), [templates](templates.md),
[verify](verify.md), [evidence](evidence.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

## Spec Packages

[Acceptance](#spec-packages-1).

The target canonical spec is one `specs/<label>/` package with `spec.md` and
`tests.md`, plus an optional `model.qnt` and supporting files. Document roles
and acceptance syntax are owned by
[spec conventions](../docs/spec-conventions.md#spec-packages); phase context is
owned by
[Templates](templates.md#acceptance-context-and-progressive-disclosure). Labels
remain stable within a package relocation; intentional cross-label extraction
follows [explicit rebinding](#cross-label-relocation). Discovery, indexing,
annotation locations, relative links, and cache rebuilding agree on this package
boundary. There is no permanent parallel flat-file discovery mode.

Every package has exactly one spec-index row. Missing required documents,
ambiguous packages, duplicate/unindexed labels, and index/path disagreement are
errors rather than partial discovery. Relocation preserves acceptance claims and
verifier bindings. Package change tracking includes contracts, criteria, models,
imports, and tracked supporting verification inputs, so a model-only edit
participates in changed-spec decomposition even without a Markdown diff.

### Cutover ordering

[Acceptance](#spec-packages-1).

Keep contracts in the flat bootstrap authoring form defined by
[spec conventions](../docs/spec-conventions.md#bootstrap-authoring-before-cutover)
until package-aware discovery, index/annotation parsing, change tracking, cache
rebuilding, queries, and phase context loading are implemented and tested.
Physical relocation then updates the index and references atomically with the
supported layout. It must not strand todo/loop context or silently empty Gate's
acceptance inventory. This ordering does not add a permanent dual-layout mode.

### Task acceptance references

[Acceptance](#task-acceptance).

Ordinary implementation tasks refer to assigned criteria using the existing
typed pair `(SpecLabel, CriterionId)`, not line numbers, heading links, or a
second identifier scheme. The
[criterion identity algorithm](#criterion-status-surface) is owned here;
Templates only consumes and renders the resulting values. Package relocation and
verifier-only edits preserve reference identity; changes to normalized
requirement wording require deliberate rebinding. The driver does not silently
retarget by similarity or follow a changed requirement under an unresolved
reference.

[Loop](loop.md#task-acceptance-at-dispatch) applies this boundary to ordinary
tasks from every producer, not only Todo. Findings owns the distinct
[remediation goal](findings.md#remediation-task-acceptance) used to repair
missing or malformed acceptance; it is not an empty or fabricated criterion
assignment, nor a fallback when an ordinary reference fails.

#### Dispatch resolution

[Acceptance](#task-acceptance).

At the dispatch boundary, the driver parses external references into typed
identifiers, then resolves every declared reference against the current package
snapshot selected for dispatch. Resolution produces an immutable acceptance set
containing the reference, current criterion text, and current verifier binding.
Parsed references and resolved obligations are distinct types: downstream
dispatch and prompt construction consume the resolved form, not raw strings or
references accompanied by a success flag. Construction cannot succeed with
unresolved members or an empty placeholder standing in for failed resolution.

Malformed identifiers, unknown packages, missing criteria, or ambiguous
resolution return typed errors before spawning the worker. No failed member is
silently dropped from the set. A changed dispatch snapshot requires fresh
resolution; it cannot inherit the old snapshot's resolved context. A surviving
reference uses the current verifier binding, not a copied historical annotation
or cached status. Resolving task acceptance does not grant verification evidence
or publication authority. A pending criterion can resolve while its verifier has
not been implemented: the binding here is parsed metadata, not an admitted
executable verifier or a passing result. [Verify](verify.md) owns executable
admission and [Evidence](evidence.md) owns result admission.

[Templates](templates.md#acceptance-context-and-progressive-disclosure) delivers
these resolved obligations explicitly and restores the dispatched context after
compaction. Ordinary Markdown contract-to-acceptance links remain the separate
author-facing navigation surface.

### Criterion-Status Surface

[Acceptance](#criterion-status-surface-1).

`criterion_status` identifies current criteria and their historical verifier
observations. The driver parses the changed specs' Success Criteria, computes
typed criterion ids, and joins `.loom/cache.db`'s observation rows. Cache
absence is missing evidence, never no work. The observation shape below is not a
current-admission certificate: `Current` means the annotation matches, not that
the result remains usable.

Alongside these observations, the driver supplies a typed per-criterion
[coverage projection from Evidence](evidence.md#coverage-projection), resolved
against current requirements, policy, execution context, and retained safety
facts. Decomposition and status consumers can distinguish observed outcomes,
established current coverage, unresolved blockers, and coverage that cannot be
established. No commit-distance test, matching annotation, or serialized status
row can construct an admitted coverage value. The projection does not run
verifiers, turn a reuse restriction into a failed criterion, or grant
publication authority.

```rust
pub struct CriterionStatus {
    pub spec_label: SpecLabel,
    pub criterion_id: CriterionId,
    pub criterion_text: String,
    pub annotation: CriterionAnnotation,
    pub evidence: EvidenceState,
}

pub struct CriterionId(/* opaque */);

pub struct CriterionAnnotation {
    pub tier: AnnotationTier,
    pub target: AnnotationTarget,
    pub pending: bool,
}

pub enum AnnotationTier {
    Check,
    Test,
    System,
    Judge,
}

pub struct AnnotationTarget(/* opaque */);

pub enum EvidenceState {
    Current {
        result: CriterionResult,
        last_timestamp_ms: i64,
        last_commit: GitSha,
        commits_since: u32,
    },
    Missing,
    StaleAnnotation {
        cached_annotation: CriterionAnnotation,
        last_timestamp_ms: i64,
        last_commit: GitSha,
        commits_since: u32,
    },
}

pub enum CriterionResult {
    Pass,
    Fail,
    Skipped,
}
```

`CriterionId` identifies the requirement, not the verifier binding. The parser
computes it from canonical bytes containing `spec_label` plus the normalized
criterion text (bullet marker stripped, continuation lines joined with single
spaces, surrounding whitespace trimmed, internal whitespace collapsed, actual
annotation spans excluded). Markdown item boundaries delimit the text; following
comments, headings, and sections are not part of the requirement. Formatting
comments inside the item are excluded too. Annotation placement on its own line
or inline does not change the requirement; examples in code remain requirement
text, not executable bindings. Under
[spec conventions](../docs/spec-conventions.md#verification-strategy), attached
strategy blocks are metadata parsed separately from requirement text. Changing
that metadata, annotation tier, or target does not make a new requirement id.
Current strategy still governs Gate planning and evidence admission; stable
requirement identity is not permission to retain old policy. Stale verifier
evidence is represented by `EvidenceState::StaleAnnotation` instead. Duplicate
normalized criterion text inside one spec is an integrity error because it would
collide.

Criteria with no annotation, multiple annotations, or malformed annotation
syntax block todo preflight. They do not appear as normal `CriterionStatus` rows
because the acceptance surface is broken.

### Cross-label relocation

[Acceptance](#cross-label-relocation-1).

Moving a package without changing its label preserves criterion identity. Moving
a requirement to another label yields its natural new label-dependent
CriterionId; existing task references require explicit rebinding, with no fuzzy
retargeting or permanent legacy alias registry. Relocation preserves each
requirement and its verifier binding. Finding references, suppression/status
attribution, and claim-linked safety history must be accounted for explicitly
rather than silently detached.

## Success Criteria

### Planning, coverage, and strategy

- Contract-to-acceptance mappings use ordinary Markdown links to named sections
  in the owning package's `tests.md`; missing destinations or heading fragments
  fail integrity validation. Resolution needs no custom mapping registry and
  does not treat a heading link as a verifier binding or criterion identity.
  [test](package_contract_links_resolve_to_acceptance_sections)

- Optional `loom-verify` fences in `tests.md` are parsed as typed TOML strategy
  declarations, distinct from ordinary code examples. A criterion-stage block
  belongs inside its owning bullet directly after the annotation; containment,
  not a repeated target/ID or nearest-annotation heuristic, determines its
  owner. Strategy metadata is separate from criterion text and verifier
  bindings. The optional stage exception is `defer_until = "publication"`, not a
  stage allowlist or disable switch. Invalid TOML, unsupported values, unknown
  fields, conflicting declarations, or detached/ambiguous criterion-stage blocks
  fail validation. Absence retains criteria and baseline obligations; local
  declarations do not become package-wide defaults.
  [test?](quint_verify_blocks_parse_optional_strict_strategy)

- Package-level strategy parsing accepts `[rust].property_filters` as an array
  of nextest expression strings and repeatable `[[role_exception]]` tables with
  `subject`, `role`, and `reason`. It rejects malformed field types and retains
  typed references and source context for separate inventory resolution; the
  ordinary TOML example in conventions does not become active policy.
  [test?](quint_verify_blocks_parse_property_filters_and_role_exceptions)

### Annotation parsing

- Parser discovers criteria in each canonical package's `tests.md` in lexical
  package order, without treating supporting Markdown or model files as
  additional acceptance documents.
  [test](parse_walks_canonical_package_acceptance_documents)

- Parser aggregates criteria across packages into a single `ParsedSpecs`,
  retaining package identity and each annotation's actual source document.
  [test](parse_aggregates_criteria_across_packages)

- Parser returns a typed read-directory error when the specs directory is
  missing rather than producing an empty result
  [test](parse_returns_read_dir_error_for_missing_directory)

### Spec packages

- Spec discovery accepts the canonical `specs/<label>/{spec.md,tests.md}`
  package, preserves labels, rejects ambiguous or incomplete packages, and does
  not retain a parallel flat-file discovery mode.
  [test](quint_package_discovery_is_canonical)

<!-- prettier-ignore -->
- Physical package cutover follows tested package-aware discovery, annotation
  parsing, change tracking, cache rebuilding, CLI queries, and plan/todo/loop
  context loading. Relocation preserves labels, acceptance claims, and verifier
  bindings; each package has one index entry, references resolve, and the real
  gate and workflow paths retain a non-empty expected acceptance inventory
  rather than silently skipping it or loading missing files. [system?](nix run .#test-quint -- package-migration)

- Changes to package contracts, criteria, models, imports, or tracked supporting
  verification inputs participate in changed-spec decomposition; a model-only
  edit cannot disappear because no Markdown file changed.
  [test?](quint_package_change_tracking_includes_model_inputs)

### Task acceptance

- Task references retain identity across package relocation and verifier-only
  edits. Changes to normalized requirement wording leave old references
  unresolved until explicitly rebound; no text-similarity fallback substitutes a
  different requirement.
  [test](task_acceptance_references_require_rebinding_after_requirement_edits)

- Parsing task references and resolving them against a package snapshot are
  distinct fallible typed transitions. Only complete immutable resolved
  obligations, carrying identity, current text, and current binding, can enter
  downstream dispatch context; raw values, booleans, and partial-result
  placeholders cannot stand in for them.
  [test](task_acceptance_resolution_produces_typed_obligations)

### Workflow commands

- `loom spec` queries spec annotations (`[check]` / `[test]` / `[system]` /
  `[judge]`) parsed via `loom-gate`'s annotation parser
  [test](list_for_label_reads_all_four_tiers)

- `loom spec <label> --deps` walks file-shaped `[test]`/`[judge]` targets and
  `[check]`/`[system]` command strings in the named spec, printing the required
  nixpkgs [test](deps_for_label_walks_file_targets_and_command_strings)

- `loom spec <label> --targets` prints one entry per annotation as
  `[tier] target`; `--tier <tier>` narrows to that tier; `--plain` prints exact
  target strings without the `[tier] ` prefix. Embedded target newlines are
  preserved, so entries need not occupy one physical line
  [test](spec_targets_lists_annotation_targets_with_tier_and_plain_modes)

### Cache database

- `CacheDb::rebuild` populates `specs` from `docs/README.md`'s spec index and
  cross-checks canonical packages; unindexed/incomplete packages, missing
  indexed packages, duplicate labels, and label/path mismatches fail loud
  [test?](cache_rebuild_cross_checks_spec_index_and_packages)

- `criterion_status` cache rows join to current criteria by typed
  `(SpecLabel, CriterionId)`; stale annotation evidence renders as
  `EvidenceState::StaleAnnotation`, absent rows as `EvidenceState::Missing`
  [test](todo_missing_criterion_cache_rows_are_missing_evidence)

- `CacheDb::rebuild` parses each spec's `## Companions` section and writes one
  `companions` row per listed path; specs without the section contribute zero
  rows (not an error) [test](cache_db_rebuild_companions)

### Criterion-status surface

- Criterion-status construction pairs historical observations with a typed
  current coverage projection. Disqualifying same-commit policy/trust/history
  changes and unresolved counterexamples prevent a cached pass from appearing as
  admitted coverage; unavailable admission is explicit, not success or a
  fabricated failure. Projection never executes verifiers or grants publication
  authority.
  [test?](criterion_status_separates_observation_from_admissible_coverage)

- Criterion text ends at its actual Markdown item boundary, not the next
  criterion or end of file; unrelated prose, headings, and following sections
  never become requirement text.
  [test](criterion_text_stops_at_markdown_item_boundaries)

- Formatting comments before, between, or inside criteria do not change their
  normalized requirement text or identity.
  [test](formatting_comments_do_not_change_criterion_identity)

- Multiline verifier annotations with balanced parentheses are excluded as
  complete spans from requirement text while preserving their exact target bytes
  and any following requirement qualifier.
  [test](multiline_balanced_bindings_do_not_leak_into_criterion_text)

- Todo uses the structurally extracted requirement text and retains the same
  typed identity and matching cached evidence after presentation-only wrapping
  or comment changes.
  [test](todo_criterion_identity_survives_markdown_formatting)

- Cached evidence keyed to an incorrectly extracted requirement is not silently
  reassigned to the corrected criterion identity; absent matching evidence is
  reported as missing.
  [test](todo_does_not_reuse_evidence_for_contaminated_criterion_ids)

- Criterion identity derived through the production parser and criterion-status
  construction excludes attached verification-strategy metadata. Adding,
  editing, or removing a local stage declaration preserves the requirement ID
  when the normalized requirement text is unchanged; changing that text still
  changes the ID. Strategy metadata remains available for current-policy
  interpretation rather than being discarded as irrelevant.
  [test?](criterion_identity_excludes_strategy_metadata)

- `CriterionStatus` is a struct with fields `spec_label`, `criterion_id`,
  `criterion_text`, `annotation`, and `evidence`; `EvidenceState` is a tagged
  enum with variants `Current`, `Missing`, and `StaleAnnotation`
  [test](criterion_status_public_shape_carries_annotation_and_evidence_states)

### Cross-label relocation

- Cross-label extraction yields natural new criterion identities and rejects old
  task references until explicitly rebound; within-label package moves preserve
  identity and bindings.
  [test](cross_label_claim_relocation_requires_explicit_rebinding)

### Package size

<!-- prettier-ignore -->
- Every indexed package's combined `spec.md` and `tests.md` is at most 2,000
  lines; splitting the two documents does not create independent budgets.
  [check](cargo run -p loom-walk -- spec_package_size)

## Requirements

### Functional

1. **Spec label parsing** — workflow commands that accept spec labels parse them
   into `SpecLabel` values at the CLI boundary. No command falls back to a
   `current_spec` cache key: `loom plan` labels are optional anchors,
   `loom todo` discovers specs from durable cursors, and `loom loop` executes
   work roots. `loom gate` is not a spec-scoped surface; gate affectedness comes
   from work scopes and target discovery uses `loom spec <label> --targets`.

2. `loom spec <label> --deps` parses the named spec's `[check]` / `[test]` /
   `[system]` / `[judge]` annotations, opens each referenced verifier source,
   and prints the deduplicated set of nixpkgs needed

### Package size

[Acceptance](#package-size).

The combined contract and acceptance documents are at most 2,000 lines, under
[the authoring ceiling](../docs/spec-conventions.md#length-guidance). Split
cohesive ownership rather than dropping requirements or hiding normative
material in appendices. Cross-label extraction follows the rebinding and
evidence rules above.

## Out of Scope

- Todo owns decomposition assignment persistence; Findings owns remediation
  goals, and Loop owns the common task-dispatch boundary. Executable admission
  belongs to Verify and evidence eligibility to Evidence. Package relocation
  does not implement permanent legacy aliases or preserve authority through
  relabeling.

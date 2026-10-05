# Verifier planning and execution

Derives independent obligations, admits checked providers, selects affected
work, and executes verifiers without weakening coverage.

## Problem Statement

Derives independent obligations, admits checked providers, selects affected
work, and executes verifiers without weakening coverage. This package is one
contract owner, not a new crate or command tree.

## Architecture

Inputs, outputs, and trust boundaries are stated in the contracts below. Related
owners: [specs](specs.md), [evidence](evidence.md), [gate](gate.md),
[tests](tests.md), [simulation](simulation.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Verification planning

[Acceptance](#planning-coverage-and-strategy).

The trust-bearing request consists of a repository snapshot, requested scope,
and validated stage. The gate derives applicable obligations from current
contracts and policy, then joins them to exact verifier targets and admitted
execution units. A caller-picked test list, provider inventory, or cached target
set cannot substitute for required coverage. Exact-target diagnostics remain
non-authorizing.

Package `tests.md` owns coverage and strategy under
[spec conventions](../docs/spec-conventions.md#verification-strategy).
`loom.toml` supplies execution wiring, not a second policy authority. Missing or
changed wiring cannot erase required obligations. Pending declarations retain
their separate pending policy.

Baseline coverage, conservative stage defaults, and evidence admission are
built-in behavior for supported Rust/Nix workflows, not opt-in safety settings.
Ordinary use requires no custom strategy block or manually maintained input
list; providers still supply admitted execution definitions. Missing optional
policy cannot silently remove required work. Invalid declarations or missing
required execution contracts fail admission instead of selecting a permissive
mode. Observed slowness never automatically defers or drops an obligation:
performance is improved through sound selection, sharing, reuse, or an explicit
spec-owned exception. A fast-feedback success is not publication authorization.

Repository-wide subject discovery is independent of verifier declarations and
cached results. Built-in Rust/Nix policies derive roles and baseline obligations
from actual project definitions: owning-target build/lint/applicable tests for
Rust, evaluation/check responsibilities for Nix, responsible consumers/producers
for fixtures/generated inputs, and document obligations for docs/specs. Projects
declare justified exceptions and additional roles, not a duplicate per-file
registry. Unclassified subjects and missing required coverage block trusted
success. This accounts for responsibilities; it does not establish semantic
adequacy or require a separate test per file.

A verification unit maps one admitted execution to the obligations it can
discharge. Units may batch many criteria or tests; a derivation per annotation
or native test is not required. [Verifier inputs](#verifier-inputs-1) owns the
tracked/constructed dependency boundary. [Verify](#selective-property-campaigns)
owns property grouping and shared native artifacts.

For Nix-enabled consumers, external routine deterministic verification uses
hermetic Nix units with pinned tools, explicit sources/fixtures, controlled
environments, and no undeclared network access. Loom-owned analysis uses tracked
inputs. Invoking a host application through `nix run` does not itself establish
hermeticity. Host services, containers, kernel behavior, and LLM review use an
explicit effectful lane where those effects are inherent; capabilities,
environment assumptions, and applicability remain visible.

Discovered obligations, resolved plans, observed results, admissible evidence,
and publication authorization are distinct typed boundaries. Unchecked strings,
booleans, partial results, or mutation of validated fields cannot construct a
later trusted state. Applying
[Harness's parse/resolve discipline](harness.md#parse-dont-validate), admission
constructs richer snapshot-bound values or typed errors rather than returning
validation flags beside unchanged input data. Downstream stages consume those
values; invalidated context requires re-deriving the affected state. These
boundaries constrain the implementation; the
[whole-gate model and production conformance](simulation.md) check sequencing,
input construction, optimization, and authorization against an independent
reference procedure.

#### Provider authoring and description boundary

[Acceptance](#admitted-inputs-and-selection).

The normal authoring path is one checked definition: tracked Rust checks
register through the shared input boundary, and deterministic Nix checks use
checked execution/resource constructors. The definition supplies execution and a
typed JSON unit description through the existing runner-query mechanism; a
built-in provider can supply the equivalent value directly. Authors do not
maintain a second input table, hand-author unit-description JSON, or implement a
new plugin protocol for an ordinary supported check.

Descriptions identify the exact target and execution definition, offered subject
responsibility, resources and source origins, discovery/observation contract,
execution requirements, and result mapping. Nix resource handles retain origins
while constructing the actual staged sources and transitive execution graph;
querying Nix afterward does not magically reconstruct missing provenance.
Descriptions reference that graph rather than independently asserting a smaller
input closure. They do not define repository-wide obligations, declare their own
completeness, or constitute verification results.

Parsing produces typed candidate descriptions, not admitted units or authority.
Fallible resolution against the current snapshot, independent inventory, and
actual provider definition constructs immutable admitted values or typed errors.
Downstream planning consumes the resolved form, not raw metadata with validation
flags. Unknown or incompatible shapes, duplicate/ambiguous targets, and
inconsistent definitions cannot be silently normalized into an admissible plan.
Valid cold observation collection remains distinct from invalid registration.

Shipped authoring guidance exposes minimal working Rust and Nix examples through
normal project/API documentation, including a failing example and its
correction. Those examples use the production constructors and query/admission
path, not a mock protocol. New checks follow the documented defaults without
reverse engineering private registries. Raw I/O outside the tracked boundary
remains a boundary violation to enforce and test; neither JSON nor Rust types
alone prove that arbitrary checker code has no hidden reads or enforces the
intended rule.

#### Corrective feedback

[Acceptance](#admitted-inputs-and-selection).

Detect malformed definitions and admission failures at the earliest boundary
that has the necessary information, including ordinary local verification rather
than waiting for publication. Work needed for discovery, evaluation, or building
an authoritative descriptor can still be necessary; this is not a promise of
compile-free discovery or a fixed latency threshold. Preserve hook ordering and
re-derive snapshot-dependent admission when hooks change relevant state.

Once an admission failure is known, do not dispatch that invalid plan, start
review on its supposed success, or construct authorization. Surface the failure
without waiting for unrelated verification merely to finish reporting.
Diagnostics identify the target and owning annotation, the producer
definition/resource when available, the specific cause, and an actionable
correction or investigation with an existing verification command to retry.
Distinguish invalid contracts, unavailable capabilities, valid cold collection,
and actual verifier failures; do not recommend an always-run fallback, weaker
scope, or bypass as the repair.

The actionable cause is visible in normal command/driver feedback, not only in
verbose output or tracing. Use existing output and event channels, preserve
machine-readable query output, and bound/redact supporting tool diagnostics.
Correcting the definition and rerunning re-enters ordinary parsing/admission; no
override flag or separate repair gate is needed. Pending claims and accepted
worker capability skips retain their existing policies and cannot mint
authority.

### Selective Property Campaigns

[Acceptance](#property-based-testing).

Discovered Rust tests default to the ordinary broad suite. Explicit
nextest-style filters in the owning package's `tests.md` select property-group
members from the current inventory; unmatched tests remain in the ordinary
suite. Filters may select tests within shared binaries or entire binaries,
without a separately maintained individual-target list or property-name
heuristic. Strategy syntax and authority are owned by
[spec conventions](../docs/spec-conventions.md#verification-strategy).

Property campaigns have individually selectable exact execution/result
identities with admitted dependencies under [Verify](#verifier-inputs-1).
Classification cannot silently drop a test or reduce mandatory publication
coverage. Ordinary unit/integration suites need not be fragmented for finer
selection.

Properties can share libraries, binaries, and compilation while retaining their
own runtime fixtures and campaign parameters. An independent runtime fixture may
invalidate only one property; a shared executable-input change may correctly
invalidate several. Neither crate splitting nor precise function-level semantic
impact analysis is required. Build/result reuse and stage applicability follow
Gate; selecting feedback work is not redefining full-publication obligations.

## Success Criteria

### Planning, coverage, and strategy

<!-- prettier-ignore -->
- Independent repository inventory and role policy account for all subjects and
  their required obligations. An unclassified file, unsupported new source kind,
  orphan production source, or missing required verifier blocks trusted success;
  fixture/generated/documentation roles receive their own policy, not implicit
  coverage from an unrelated verifier. [system?](nix run .#test-quint -- coverage-admission)

- Built-in Rust/Nix role policies derive baseline obligations from project
  definitions: owning-target build/lint/tests for Rust, evaluation/checks for
  Nix, responsible consumers/producers for fixtures/generated inputs, and
  document checks for documentation/specs. Ordinary subjects require no second
  per-file registry; exceptions and additional roles are explicit spec
  declarations. [test?](quint_builtin_role_policies_derive_baseline_obligations)

<!-- prettier-ignore -->
- Verification strategy and coverage declarations, including property filters
  and justified role exceptions, are owned by `tests.md`. Changes to `loom.toml`
  execution wiring cannot override those declarations or remove required
  targets; missing wiring remains an admission failure, not reduced coverage.
  Pending declarations retain their separate policy under spec conventions.
  [system?](nix run .#test-quint -- spec-policy-authority)

<!-- prettier-ignore -->
- Supported Rust/Nix projects with no custom strategy blocks retain baseline
  coverage and ordinary affected checking through the production gate. Removing
  an optional exception cannot omit publication work; malformed declarations,
  unsupported stage values, or missing required provider contracts cannot yield
  permissive success. No tuning flag or timeout converts required work into an
  implicit deferral or pass. [system?](nix run .#test-quint -- default-policy)

### Admitted inputs and selection

- An executable verifier resolves to an exact target, its subject inventory and
  input-discovery contract, its execution requirements, and its result mapping.
  Missing contracts, ambiguous targets, malformed metadata, or discovery
  failures are configuration failures, not silent always-run fallback or
  permission to skip. Pending declarations retain the separate pending policy
  defined by spec conventions.
  [test?](quint_verifier_admission_requires_resolved_scope)

- Intentionally broad or global verification is explicit and justified; it is
  distinguishable from failed discovery in the execution plan.
  [test?](quint_global_scope_is_explicit)

- Registration, generated descriptions/input queries, and execution derive their
  subjects and inputs from one checked definition, rather than independent
  registries or hand-authored input metadata.
  [test?](quint_verifier_definition_drives_discovery_and_execution)

<!-- prettier-ignore -->
- Production Rust/Nix constructors expose generated typed unit descriptions
  through the existing query/provider path, retaining exact target identity,
  responsibility, actual execution dependencies, and source origins. Parsing is
  not admission: inconsistent metadata or an input-only assertion cannot replace
  resolution against the current provider definition and independent inventory.
  Single-target and batched queries retain complete per-target attribution.
  [system?](nix run .#test-quint -- provider-descriptions)

<!-- prettier-ignore -->
- Published minimal Rust/Nix authoring examples use production constructors and
  default verification entrypoints to add and run a check without a second input
  registry or hand-authored metadata. Their documented failure/correction
  example is executable and returns through normal admission, not a test-only
  shortcut. [system?](nix run .#test-quint -- provider-authoring)

<!-- prettier-ignore -->
- Ordinary verification reports provider/admission errors as soon as the failing
  boundary has enough information, without dispatching the invalid plan or
  waiting for unrelated verification/review to supply the actionable cause.
  Normal feedback identifies the target, source context, cause, and correction
  or investigation/retry path. Corrected definitions can succeed through the
  same path; cold collection and capability limits are not mislabeled as invalid
  contracts, and no bypass converts failure into success. [system?](nix run .#test-quint -- provider-admission-feedback)

<!-- prettier-ignore -->
- Loom-owned checks consume tracked provider inputs; traces record membership,
  contents, existence, path resolution, and relevant configuration/checker
  identity. Provider-conformance cases expose deliberately omitted reads,
  including new files and formerly absent configuration. [system?](nix run .#test-quint -- tracked-inputs)

<!-- prettier-ignore -->
- Selective external execution and reuse require checked constructors whose
  actual staged sources and transitive execution dependencies correspond to the
  admitted definition. An arbitrary self-reported smaller input list cannot
  authorize exclusion or reuse. [system?](nix run .#test-quint -- unit-construction)

<!-- prettier-ignore -->
- Subject discovery responds correctly to additions, deletions, renames, new
  workspace targets, and configuration changes; adversarial fixtures detect
  omitted subjects even when the resulting execution would be hermetic.
  [system?](nix run .#test-quint -- subject-completeness)

<!-- prettier-ignore -->
- Dependency-contract tests cover verifier code, manifests, lockfiles, fixtures,
  templates, generated inputs, model imports, and tool/configuration changes;
  omitted relevant inputs are detected rather than treated as unaffected.
  [system?](nix run .#test-quint -- input-completeness)

- A finite scope includes every affected eligible verifier and excludes every
  verifier established to be unaffected. Explicit global responsibility is part
  of applicability, not an exemption from declaring scope.
  [test?](quint_selection_is_complete_and_precise)

- Selection scope is distinct from analysis scope: selecting a cross-file
  verifier does not remove unchanged subjects it needs to inspect. Changed-file
  execution is used only for verifiers whose declared semantics permit it.
  [test?](quint_selection_does_not_truncate_analysis_inputs)

<!-- prettier-ignore -->
- Hermetic Nix units cannot read undeclared workspace inputs or use undeclared
  network access; changes to declared inputs or execution definitions invalidate
  their result identity. [system?](nix run .#test-quint -- hermetic-inputs)

<!-- prettier-ignore -->
- Input partitioning preserves complete subject coverage without attaching the
  whole repository to every unit by default; independent verifier inputs permit
  independent invalidation. [system?](nix run .#test-quint -- independent-units)

<!-- prettier-ignore -->
- An inspectable execution plan reports selection/exclusion reasons, scope and
  dependency provenance, shared executions, reuse decisions, and unresolved
  requirements; completed runs report observed durations and invocation counts.
  [system?](nix run .#test-quint -- explain-plan)

### Execution and admissible evidence

- Batching preserves obligations and per-criterion results. Equivalent eligible
  invocations share execution, while different scenarios, inputs, environments,
  scopes, or freshness requirements are not conflated.
  [test?](quint_batching_preserves_execution_identity_and_results)

- A required check with a valid provider but no admissible prior result
  executes; missing tracked observations are collected rather than treated as an
  empty dependency set. Unusable prior evidence never authorizes skipping, and
  missing provider contracts remain admission failures rather than cold runs.
  [test?](quint_cold_execution_is_distinct_from_invalid_admission)

- Current validated inventory determines obligations independently of cached
  targets: new tests, subjects, and required lanes cannot be omitted because no
  cache entry exists for them.
  [test?](quint_cached_targets_do_not_define_required_coverage)

- Effectful checks declare capabilities and applicability; unavailable,
  inapplicable, failed, pending, skipped, and passed outcomes remain distinct. A
  selected required check that did not pass cannot become passed through
  aggregation or a successful wrapper exit code.
  [test?](quint_effectful_outcomes_do_not_collapse_into_success)

- Rust API boundaries distinguish discovered obligations, resolved executable
  plans, observed results, policy-admissible evidence, and authorization;
  callers cannot construct downstream trusted states from unchecked strings,
  booleans, partial results, or mutable validated fields. Deserializing a unit
  description cannot construct an admitted unit; only current-context resolution
  produces the immutable form consumed by planning.
  [test?](quint_evidence_boundaries_reject_invalid_transitions)

### Integrity gate — four directions

- **Forward — baseline.** A spec with all valid annotations yields no findings
  [test](parse_then_check_with_all_valid_annotations_yields_no_findings)

- **Forward — broken targets per tier.** Each tier flags its own broken-target
  shape: `[check]` first token absent on PATH, `[test]` path with no matching
  function, `[judge]` file absent
  [test](fixture_with_broken_target_per_tier_flags_each_one)

- **Forward — judge `#fn` selector.** A `[judge](script#fn)` target resolves
  when the leading script path exists; the `#fn` suffix is stripped before the
  on-disk check (per the _Verifier inputs_ table's `[judge](script#fn)` row).
  `::fn` is accepted during migration but `#fn` is canonical because the
  URL-fragment shape is what markdown renderers click through to
  [test](forward_judge_accepts_script_with_hash_fn_selector)

- **Forward — judge spec-relative resolution.** Path resolution joins the
  relative target against the annotation's spec-file directory, not the repo
  root; absolute paths are honoured as-is. This matches the markdown renderer's
  relative-link resolution so a clickable `[judge](../tests/judges/x.sh#fn)` in
  `specs/foo/tests.md` resolves to `tests/judges/x.sh` on disk
  [test](forward_judge_resolves_relative_to_spec_dir)

- **Forward — judge legacy `::fn` selector.** A `[judge](script::fn)` target
  still resolves during the `::` → `#` migration; the `::fn` suffix is stripped
  before the on-disk check [test](forward_judge_accepts_script_with_fn_selector)

- **Forward — system `::attr` selector.** A `[system](path::attr)` target (e.g.
  `[system](tests/unit.nix::eval-smoke)`) resolves when the leading path exists;
  the `::attr` suffix is stripped before the PATH / file check, matching the
  `[judge]` shape [test](forward_system_accepts_path_with_attr_selector)

- **Forward — test-tier missing function.** A `[test](cargo test …)` annotation
  whose test name does not match any function in the workspace is flagged
  [test](check_flags_cargo_test_annotation_with_missing_test_name)

- **Stub-pointing.** A `[test]` annotation whose body invokes the
  `_pending_stub` sigil is flagged as `StubTestFunction`
  [test](stub_pointing_test_annotation_flags_via_workspace_scanner)

- **Atomic-acceptance.** Two annotations on one criterion flags
  `MultipleAnnotations`
  [test](two_annotations_on_one_criterion_flags_atomic_acceptance)

- **End-to-end.** A specs directory containing both broken-target and
  multiple-annotation fixtures surfaces findings from both directions in one
  pass [test](end_to_end_specs_dir_check_combines_both_directions)

- **Verify lane — terminal.** Integrity findings exit non-zero from
  `loom gate check` and `loom gate verify` runs (the integrity gate is itself a
  `[check]`-tier verifier, so it fails the same way the per-annotation `[check]`
  dispatch does)
  [test](gate_check_fails_on_integrity_finding_for_unresolved_annotation)

- **Self-hosting — check tier.** The integrity gate accepts a `[check]`
  annotation in `specs/verify/tests.md` whose first token resolves on PATH
  (closes the bootstrap concern: the spec that defines the taxonomy can carry
  its own annotations)
  [test](self_referential_check_annotation_resolves_against_integrity_gate_implementation)

- **Self-hosting — judge tier.** A `[judge]` annotation in
  `specs/verify/tests.md` pointing at the integrity gate's own
  `src/integrity.rs` resolves
  [test](self_referential_judge_annotation_resolves_against_integrity_source_file)

### Integrity gate — pending modifier

- **Parser recognises `?` modifier — all tiers.** `[check?]`, `[test?]`,
  `[system?]`, `[judge?]` all parse; the parser populates a `pending: bool` on
  the resulting annotation
  [test](parse_recognises_pending_modifier_for_all_four_tiers)

- **Pending — unresolved target silently passes.** `[check?](missing-cmd)`,
  `[test?](missing::fn)`, `[system?](missing-system-cmd)`,
  `[judge?](missing/path.md)` all clear forward resolution with no finding
  [test](pending_marked_unresolved_target_yields_no_finding)

- **Pending — resolved target emits `UnneededPendingMarker`.** A
  `[check?](true)` (where `true` is on PATH) flags the stale marker, naming
  spec + line + target
  [test](pending_marked_resolved_target_yields_unneeded_pending_marker)

- **Pending `[test?]` — stub body silently passes.** The modifier suppresses
  `StubTestFunction` the same way it suppresses `UnresolvedAnnotation`
  [test](pending_marked_stub_test_body_yields_no_finding)

- **Pending `[test?]` — non-stub body emits `UnneededPendingMarker`.**
  Co-incidence with target resolution forces the implementing diff to drop `?`
  at the same commit that lands the real body
  [test](pending_marked_non_stub_test_body_yields_unneeded_pending_marker)

- **Atomic-acceptance — `?` does not suppress.** Two annotations on one
  criterion still flag `MultipleAnnotations`, whether either, both, or neither
  carries `?`
  [test](pending_modifier_does_not_suppress_atomic_acceptance_finding)

- **`UnneededPendingMarker` — terminal at push gate.** Surfaces alongside
  `UnresolvedAnnotation` and `StubTestFunction` per the _Findings and Minting_
  emit-shape table [test](unneeded_pending_marker_is_terminal_at_push_gate)

- **`unneeded-pending-marker` — auto-generated options.** `mint` emits a
  `## Options — …` block whose Option 1 is "drop the `?`", per _Integrity gate_
  above [test](mint_emits_drop_marker_option_for_unneeded_pending_marker)

### Verifier inputs

- `[test]` input resolution uses an admitted native provider and includes the
  applicable spec contract; Cargo target metadata alone is not accepted as the
  complete execution-input closure.
  [test?](test_tier_resolution_requires_provider_closure_and_spec_contract)

- A `[judge]` target is located by shared selector-stripping (`#fn` / `::fn` /
  `::attr`) plus spec-relative resolution, the same helper the integrity gate
  uses, so the input resolver and integrity gate cannot disagree about where the
  script lives
  [test](judge_tier_strips_selector_and_collects_relative_to_spec_dir)

- A judge script reports per-function inputs via `<script> --print-inputs <fn>`
  collect mode — `judge_files` records its path arguments while
  `judge_criterion` and the LLM call are skipped, and the recorded paths are
  emitted as `{"inputs":[...]}`
  [test](judge_collect_mode_records_judge_files_paths)

- The judge collect-mode batch projection maps each rubric to its globs in one
  spawn — `<script> --print-inputs` with no `<fn>` emits
  `{"inputs":{"<fn>":[...]}}` for every rubric, rather than querying per rubric
  [test](batch_print_inputs_maps_each_target_to_its_globs)

- The `--print-inputs` query is issued through the verifier's command template,
  not by prepending the flag to the command's first token, so a
  `cargo run -p <crate> -- <walk>` verifier is queried as the walk's own
  argument [test](print_inputs_issued_through_command_template_not_argv_head)

- A declared provider input-query that exits non-zero or emits malformed
  metadata is flagged `inputs-protocol-error`; it cannot silently fall back to a
  different provider, always-run behavior, or the spec contract alone.
  [test?](provider_input_query_failure_blocks_admission)

- A `[check]` / `[system]` target that matches a runner resolves via that
  runner, not via a `tokens[0]` PATH/file check; only an unmatched target falls
  back to the `tokens[0]` check
  [test](runner_matched_target_resolves_via_runner_not_token_path_check)

- A runner's admitted dependency observations decide its `--files` inclusion:
  affected siblings are selected and established-unaffected siblings excluded,
  with discovery batched for the matched group rather than queried per target.
  [test?](filter_by_files_uses_admitted_batched_provider_observations)

### Dispatch — per-tier process model

- Runner-matched `[check]` annotations batch into one subprocess per runner
  [test](run_check_batches_loom_walk_shaped_targets_through_one_runner_spawn)

- Unmatched `[check]` annotations use the fallback path and spawn one subprocess
  per annotation
  [test](dispatcher_spawns_one_subprocess_per_unmatched_check_annotation)

- When `[system]` invocations require execution, equivalent invocations execute
  once per gate run and fan evidence out to every owning criterion in input
  order
  [test](system_dispatch_shares_equivalent_invocations_and_fans_out_evidence)

- Shared system-runner execution preserves passes, failures, skips, structured
  metadata, producer errors, and malformed-output failures
  [test](system_runner_shares_pass_failure_skip_and_invalid_output_without_losing_owners)

- Equal command strings do not merge distinct system scenarios or runner working
  directories
  [test](system_dispatch_keeps_distinct_scenarios_and_runner_cwds_isolated)

- System dispatcher memoization is lazy and confined to one run with one
  dispatch scope/environment; a later dispatcher invocation does not inherit it
  [test](system_dispatch_is_lazy_and_never_reuses_execution_across_runs_or_scopes)

- CLI system sharing retains per-criterion reporting and cache records; skipped
  coverage stays skipped and cannot mint a marker
  [test](system_cli_sharing_preserves_per_criterion_reports_failures_skips_and_cache_records)

- Shared system dispatch errors report every owner without persisting passing
  evidence
  [test](system_cli_shared_dispatch_errors_report_every_owner_without_passing_cache_entries)

- Shared system targets selected by explicit `--target` execute anew on each
  request
  [test](system_cli_exact_shared_target_executes_again_on_a_later_gate_run)

- `[test]` tier batches every in-scope target into one runner subprocess per
  invocation [test](test_tier_batches_all_targets_into_one_runner_subprocess)

- `[test]` tier filters targets by `--files` scope intersection before invoking
  the runner [test](test_tier_filters_targets_by_files_scope_intersection)

- `[test]` tier returns no subprocess when the `--files` filter excludes every
  target [test](test_tier_returns_none_when_files_filter_excludes_everything)

- `[test]` tier returns no subprocess when no `[test]` annotations are in scope
  at all [test](test_tier_returns_none_when_no_test_annotations_in_input)

- `[judge]` tier batches every target into one runner subprocess per invocation
  [test](judge_tier_batches_all_targets_into_one_runner_subprocess)

- `[judge]` tier ignores `--files` scope filtering (unlike `[test]`)
  [test](judge_tier_ignores_files_scope_unlike_test_tier)

- Dispatcher skips annotations whose tier does not match the requested tier
  [test](check_tier_skips_annotations_with_non_check_tier)

### Dispatch — env contract

- The dispatcher sets `LOOM_FILES` and `LOOM_SPEC` env vars on every verifier
  subprocess (per _Verifier-runner contract_)
  [test](dispatcher_sets_loom_files_and_loom_spec_env_on_verifier_subprocess)

### Dispatch — JSON verdict and exit-code fallback

- `[check]` tier falls back to "exit code 0 → pass" when the verifier emits no
  JSON line (per _Fallback for non-conforming verifiers_)
  [test](check_tier_falls_back_to_exit_code_pass_when_verifier_omits_json)

- `[check]` tier falls back to "non-zero exit → fail" when the verifier emits no
  JSON line
  [test](check_tier_falls_back_to_exit_code_fail_when_verifier_omits_json)

- `[test]` runner falls back to exit code when the runner omits a JSON
  per-target line
  [test](test_tier_falls_back_to_exit_code_when_runner_omits_json_line)

- A malformed JSON verdict (e.g. `pass` field with wrong type) surfaces as a
  typed dispatch error rather than silently passing
  [test](dispatcher_surfaces_malformed_verdict_when_pass_key_has_wrong_type)

- Incidental JSON on stdout that isn't a recognised verdict line falls through
  to the exit-code path
  [test](dispatcher_falls_through_to_exit_code_on_incidental_json)

- A verifier command that fails to spawn (command not found) surfaces as a
  dispatch error — the gate-exit-2 case from the _Verifier-runner contract_
  [test](dispatcher_surfaces_spawn_failure_when_command_not_found)

### Runners — batched dispatch

- `run_with_runners` groups matched annotations into one batch per runner and
  falls back to per-annotation spawn for unmatched annotations
  [test](run_with_runners_groups_matched_into_one_batch_and_falls_back_for_unmatched)

- When multiple runners' `match` regexes could apply, the first match in spec
  order wins [test](run_with_runners_first_match_wins_in_spec_order)

- When a batched-runner invocation does not produce per-target output for every
  annotation in the batch, the missing targets surface as dispatch failures
  [test](run_with_runners_dispatch_fails_targets_missing_from_batch_output)

- Runner cwd resolution — explicit `cwd` is resolved against the repo root
  [test](run_with_runners_resolves_cwd_against_repo_root)

- Runner cwd resolution — a runner with no `cwd` falls through to the
  tier-default `cwd`
  [test](run_with_runners_falls_through_to_tier_default_when_runner_cwd_is_none)

- Runner cwd resolution — a runner with no `cwd` and no tier-default uses the
  repo root
  [test](run_with_runners_uses_repo_root_when_neither_runner_nor_tier_cwd_set)

- Tier-default `cwd` also applies to per-annotation fallback when the matched
  runner has no cwd
  [test](run_with_runners_tier_default_applies_to_unmatched_per_annotation_fallback)

- `libtest-json` parser maps test-event names back to annotation targets
  [test](run_with_runners_libtest_json_maps_test_names_back_to_annotations)

- Ignored libtest tests and skipped JUnit cases remain skipped through CLI
  dispatch, cache persistence and todo criterion evidence; mixed and all-skipped
  batches never turn skips into observed passes
  [test](skipped_tests_survive_cli_cache_and_criterion_evidence)

- Known unstructured test summaries cannot certify a whole batch when some tests
  were skipped; failures still take precedence over skips
  [test](unstructured_cargo_skips_do_not_certify_every_target)

- Nextest's filtered-out test count does not mark selected passing targets as
  skipped when explicit receipts certify every target; a selected ignored test
  still prevents an aggregate pass
  [test](nextest_dispatch_distinguishes_filtered_tests_from_selected_ignored_tests)

- Empty doctest suites do not turn executed unit tests into a zero-match
  dispatch error [test](empty_doctest_suite_does_not_erase_executed_unit_tests)

- JUnit parsing respects XML quoting, entities and CDATA rather than guessing
  outcomes from tag-shaped text
  [test](junit_preserves_skips_entities_and_cdata_without_textual_tag_guessing)

- Malformed, unsupported or duplicate-identity JUnit reports fail closed
  [test](junit_rejects_malformed_unsupported_and_ambiguous_reports)

- Malformed JUnit output cannot persist partial passing evidence
  [test](malformed_junit_fails_without_persisting_partial_passes)

- Exit 77 still parses individual batch outcomes and never conceals a failure
  [test](batch_exit_77_parses_individual_results_and_cannot_hide_failure)

- Contradictory legacy pass and skip flags are rejected rather than normalized
  [test](skipped_wire_verdict_cannot_claim_an_observed_pass)

- JSON-lines rejects malformed, duplicate, incomplete and contradictory records
  [test](parse_json_lines_rejects_malformed_duplicate_and_incomplete_records)

- Optional result fields reject present null values rather than treating them as
  omission [test](optional_result_fields_reject_present_null_values)

- Declared foreign-platform and allowlisted capability skips can be accepted by
  worker verification while tier execution remains exit 77, coverage stays
  skipped in reports/cache, and no verified scope or marker is produced
  [test](worker_accepts_foreign_platform_and_declared_capability_skips_without_verifying_them)

- Unexpected skips remain blocking even under worker capability policy
  [test](unexpected_skips_cannot_be_accepted_by_worker_policy)

- Failure+skip batches preserve individual failures even on producer exit 77
  [test](failure_skip_batches_preserve_failures_even_when_producer_exits_77)

- Failed producers and zero-exit skips cannot establish verified cache evidence
  [test](producer_failure_and_zero_exit_skips_never_create_verified_cache_evidence)

- Legacy false records remain failures regardless of skip-like evidence text
  [test](legacy_boolean_failures_never_become_capability_skips_from_evidence_text)

- Synthetic Wrix-seam producers aggregate failures before skips before passes
  [test](synthetic_wrix_producer_aggregates_failures_before_skips_before_passes)

- `exit-code` parser shares a single per-runner verdict across every target in
  the group
  [test](run_with_runners_exit_code_parser_shares_verdict_across_group)

### Annotation gate

- Every `[check]` / `[test]` / `[system]` / `[judge]` annotation in canonical
  package acceptance documents is checked under Gate's resolution and pending
  policy. [test?](end_to_end_package_specs_check_combines_integrity_directions)

### Property-based testing

- Explicit nextest-style property filters partition the current discovered Rust
  test inventory into property groups and the remaining ordinary suite. Filters
  can select members within shared binaries; new unmatched tests stay in the
  broad suite rather than disappearing or requiring a duplicate target list.
  [test?](quint_property_filters_partition_discovered_tests)

<!-- prettier-ignore -->
- Ordinary native unit/integration suites may remain broad, while property
  campaigns have individually selectable exact targets and results. Every
  discovered native test is accounted for by a suite or property group; grouping
  does not silently drop tests or alter required full-publication coverage.
  [system?](nix run .#test-quint -- property-selection)

<!-- prettier-ignore -->
- Property executions share libraries/build artifacts without requiring crate
  splits or duplicate compilation. Distinct runtime fixtures/campaign inputs
  invalidate their dependent properties; changed shared execution inputs may
  correctly invalidate several properties without implying a scope defect.
  [system?](nix run .#test-quint -- shared-property-artifacts)

### Mechanisms

- Worker acceptance with permitted capability skips cannot construct verified
  host coverage or authorize publication.
  [test](worker_accepts_foreign_platform_and_declared_capability_skips_without_verifying_them)

- Sandbox skip permission applies only to admitted foreign-platform or
  explicitly allowlisted capability gaps, not arbitrary failures.
  [test](sandbox_policy_permits_only_declared_platform_or_capability_gaps)

- Malformed, conflicting, duplicate, or incomplete JSON result batches fail
  closed without caching partial passes.
  [test](invalid_json_batches_fail_closed_without_partial_pass_cache_entries)

- Worker verification may accept declared foreign-platform or unavailable-
  capability skips while reporting those targets as unverified, never passed.
  [test](worker_accepts_foreign_platform_and_declared_capability_skips_without_verifying_them)

## Requirements

#### Mechanisms

[Acceptance](#dispatch--per-tier-process-model).

How conformance / style / test-quality are evaluated:

- **Verifier path.** A passing deterministic verifier (`[check]`, `[test]`, or
  `[system]`) exercises the claim. Deterministic, mechanical. The gate trusts
  the verifier _only if_ the test-quality dimension confirms the verifier is
  honest (Invariant 2).

- **Trace path.** An LLM trace through the consumer's current code finds the
  claim's implementation. Used when no verifier exists, or when the claim
  doesn't reduce to a single test (e.g., architectural invariants like _"loom
  never invokes `podman run` directly"_).

If both paths are available, both run. Failure on either → flag.

#### Annotation resolution

[Acceptance](#integrity-gate--four-directions).

Each criterion's annotation is resolved per its tier:

| Tier       | Target shape                                                                                                            | Dispatch                                                                                                                                                                                                                                |
| ---------- | ----------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `[check]`  | `[check](target)` — a runner identifier (matched by a `[runner.check.<name>]` block in `loom.toml`) or an argv string   | Runner-matched targets batch into one subprocess per runner and self-report inputs (see _Runners_); an unmatched target falls back to invoking its own process (often a walk binary the consumer ships).                                |
| `[test]`   | `[test](path)` — language-native test path (e.g. `crate::module::test_name`, `tests/test_foo.py::test_bar`)             | The gate collects all `[test]` targets in a single `loom gate test` invocation and issues **one** runner subprocess (e.g. `cargo nextest run -E 'test(p1) \| test(p2) \| ...'`). One process per invocation, full internal parallelism. |
| `[system]` | `[system](target)` — a runner identifier (matched by a `[runner.system.<name>]` block in `loom.toml`) or an argv string | One subprocess per equivalent invocation within a gate run, with evidence fanned out to all owning criteria. Distinct system scenarios remain separate; see _Runners_ for equivalence and input discovery.                              |
| `[judge]`  | `[judge](path)` — file path or criterion id whose content is the LLM rubric                                             | The gate collects all `[judge]` targets and issues concurrent LLM calls (API-level parallelism).                                                                                                                                        |

##### Command tokenisation

[Acceptance](#dispatch--per-tier-process-model).

`[check]` and `[system]` targets are **argv strings, not shell commands**. The
dispatcher runs `shlex::split(command)` and treats the first token as the binary
and the remainder as argv — no shell wrapper, no `/bin/sh -c`. Shell-only
constructs do not survive tokenisation and either fail at exec time or become
literal argv elements with surprising effects:

- Leading `!` (negation) — becomes `argv[0]`; exec fails with "No such file or
  directory".
- `|`, `&&`, `||`, `;` — become literal argv elements passed to the first
  binary.
- `>`, `<`, `2>&1` — same; no redirection occurs.
- `$(...)`, `` `...` `` — no command substitution; the literal text is passed.
- Globs (`*`, `?`, `[abc]`), `~`, brace expansion — no expansion; the literal
  text is passed.

Two idiomatic workarounds when an annotation genuinely needs shell semantics:

1. **Shell-free rewrite (preferred).** Re-encode the assertion using a tool that
   does the equivalent in one process. The common case — _"file does NOT contain
   pattern X"_ (the natural `! grep -q X file` form) — encodes shell-free as:
   `[check](awk 'BEGIN{found=0} /X/{found=1} END{exit found}' file)` Exits 0 iff
   `X` is absent. Same semantics, no shell.

2. **`bash -c "…"` wrapper.** When the assertion really needs a pipeline, a
   redirect, or compound logic, wrap the whole thing in a single shell
   invocation: `[check](bash -c "grep -E 'X' file | wc -l | grep -qx '3'")` The
   dispatcher sees `bash` as `argv[0]` and `-c "…"` as the remaining argv, which
   is well-formed.

Prefer the shell-free rewrite when one fits — fewer moving parts and faster (no
shell-startup overhead per invocation). Reach for `bash -c` when the natural
shell-encoding is materially clearer than any single-tool equivalent.

##### Pending modifier

[Acceptance](#integrity-gate--pending-modifier).

A `?` between the tier name and the closing `]` marks an annotation as
**pending** — its target is expected not to resolve yet because the
implementation will land in a follow-on bead. Grammar: `[tier?](target)`. The
modifier is uniform across all four tiers: `[check?](...)`, `[test?](...)`,
`[system?](...)`, `[judge?](...)`.

The pending modifier exists to let `loom plan` declare the checkable surface for
a not-yet-implemented claim and commit/push that declaration without the
integrity gate refusing the push. Without it, plan output cannot ship through
its own gate; operators face a choice between `--no-verify` bypass and
hand-curated external allowlists — neither acceptable.

Per-annotation integrity outcome:

| Modifier | Target resolves? | Outcome                                                                          |
| -------- | ---------------- | -------------------------------------------------------------------------------- |
| absent   | yes              | silent pass                                                                      |
| absent   | no               | `UnresolvedAnnotation` finding                                                   |
| `?`      | no               | silent pass (pending)                                                            |
| `?`      | yes              | `UnneededPendingMarker` finding — implementation landed; the `?` must be dropped |

For `[test?]`, the modifier additionally suppresses `StubTestFunction` findings
while the function body remains `_pending_stub`; once the body becomes real
evidence, `UnneededPendingMarker` fires the same way as for plain resolution.
The two findings both express _"implementation not present yet,"_ so a single
modifier suppresses both.

**Dispatch-side skip.** Pending-marked annotations are **skipped at verifier
dispatch** — `loom gate verify` / `check` / `test` / `system` / `judge` / `mint`
does not execute the verifier for a `[tier?](target)` annotation. Only the
integrity gate's forward-resolution check runs, which is what fires
`UnneededPendingMarker` when the target newly resolves. Without dispatch-side
skip, planning sessions that author `[check?]` for not-yet-existing walks would
break their own gate verify path on the next CI run — the verifier would
execute, exit non-zero ("command not found"), and surface as a verify-fail; the
`?` discipline would be unusable in the very flow it was added to support.

**Forward-resolution executes the command.** The integrity gate's
forward-resolution check runs the annotation's command in the same dispatch
environment as `[check]` / `[test]` / `[system]` would use for the non-pending
form, and inspects the exit code:

- Exit 0 → the assertion holds; fire `UnneededPendingMarker` (the `?` is stale
  and must be dropped in the same diff).
- Exit non-zero → the assertion does not hold; silent pass (still pending).

This broader check is what makes the `?` modifier honor the author's intent
uniformly across binary-pending (the verifier executable doesn't exist yet —
first-token-on-PATH fails) and assertion-pending (the verifier exists but the
asserted condition isn't true yet — e.g.
`[check?](grep -q 'pub enum BadWalk' crates/loom-templates/src/previous_failure.rs)`
where `grep` resolves but the symbol doesn't yet appear in the file). Both
fail-modes produce non-zero exit; both are silent-pass under the modifier. When
the implementation lands and the assertion newly holds, `UnneededPendingMarker`
fires uniformly.

Two boundary conditions:

- **Command convention is read-only.** Verifier commands are read-only by
  convention (same convention that applies to non-pending `[check]` / `[test]` /
  `[system]`). The integrity gate executes pending-marked commands during
  forward-resolution, so a side-effectful command would side-effect at integrity
  time. Authors are responsible for keeping verifier commands read-only — this
  is not a new risk class.
- **Command-broken vs assertion-pending is indistinguishable.** A command that
  exits non-zero because the implementation isn't ready and a command that exits
  non-zero because the command itself is malformed both produce silent pass. The
  integrity gate cannot distinguish them. The bug surfaces when the implementer
  drops the `?` and the verifier runs at normal `loom gate verify` — the same
  command exits non-zero and surfaces as `verify-fail`. Delayed signal during
  the pending window, not silent forever.

The modifier is **self-cleaning**. It is modelled on Rust's `#[expect(...)]`
attribute, not `#[allow(...)]`: presence is silently tolerated while the
underlying condition holds; the moment the condition resolves, the marker
_itself_ becomes the finding. The implementer who lands the verifier must drop
the `?` in the same diff or the push gate refuses on `UnneededPendingMarker`
(recoverable: the finding mints into a remediation batch and the loop re-enters
until the cap exhausts; see _Integrity gate_). This forces co-incidence between
_"target now resolves"_ and _"marker now removed,"_ so the spec tree never
carries stale markers past the molecule's push gate.

Lifecycle binding to plan → todo → loop:

- `loom plan` writes `[tier?](target)` when authoring a Success Criteria bullet
  whose verifier is not yet implemented. Applying the marker is part of the
  plan-stage Completeness check (see
  [_Plan-stage checks_](plan.md#plan-stage-checks) below).
- `loom todo` fans out beads from the spec diff as usual; pending-marked
  criteria are minted as ordinary tasks, with the integrity gate's self-cleaning
  behaviour as the only enforcement.
- `loom loop` implements the criterion. The implementer's diff drops the `?`
  from the annotation at the same time it lands the verifier;
  `UnneededPendingMarker` provides the structural enforcement that forces
  co-incidence.

**`[judge]` annotations are clickable links.** The path inside the parentheses
is read both by the gate (to dispatch a verifier) and by markdown renderers
(GitHub, VS Code, terminal viewers) when a reader clicks the link. Two
requirements compose to keep that click working:

1. **URL-fragment selector.** Shell-function selectors use `#fn` (standard
   markdown / URL fragment syntax), not `::fn`. A renderer sees `path#fn` as the
   same `path` it would for `path` alone, then scrolls to the `#fn` anchor;
   `path::fn` resolves to a literal filename ending in `::fn`, which 404s.
2. **Spec-relative path.** Paths are written relative to the spec file's own
   directory (e.g. `../../tests/judges/x.sh#fn` from `specs/<label>/tests.md`).
   The renderer's relative-link resolution and the integrity gate's resolution
   share the same base, so a path that clicks correctly in a rendered spec also
   resolves on disk for the gate. Absolute paths are honoured as-is.

`::fn` selectors are accepted during migration; new annotations use `#fn` so the
click works.

###### Pending support in structured walker input

[Acceptance](#integrity-gate--pending-modifier).

The per-annotation pending modifier above handles the common case: one SC, one
verifier target, dispatch-side skip when `?` is set. **Sweeping walkers** —
verifiers that read structured input from the spec (the pinning-matrix walker
reads templates.md's matrix table; the surface-conformance walker reads
Harness's command index; the anti-drift wire-format walker reads the canonical
partial path) and produce _per-element_ findings from a single dispatch — break
that model: the SC-level `?` can suppress the walker's dispatch entirely (only
if every SC sharing the target is `?`-marked) but cannot suppress individual
elements the walker reports inside one dispatch.

The structural fix: **a sweeping walker that reads structured input from the
spec MUST support pending element markers in that input** — two markers,
symmetric: `?` for _pending addition_ (the element will resolve to its
assertion-side present value) and `~` for _pending removal_ (the element will
resolve to its assertion-side absent value). Same self-cleaning discipline as
per-annotation `[tier?]`: the marker silent-passes during the pending window;
the moment the underlying state catches up and makes the marker stale, the
walker fails so the author drops the marker to its resolved value in the same
diff.

**Walker contract** (additive — existing two-valued walkers extend to
four-valued):

For each element in the walker's structured input, the walker checks the
element's marker against the actual workspace state:

| Marker                 | Actual state | Outcome                                                                                                                          |
| ---------------------- | ------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| present (e.g. `✓`)     | present      | silent pass                                                                                                                      |
| present                | absent       | walker failure (existing — assertion mismatch)                                                                                   |
| absent (blank)         | absent       | silent pass                                                                                                                      |
| absent                 | present      | walker failure (existing — assertion mismatch)                                                                                   |
| `?` (pending addition) | absent       | silent pass (pending — impl not yet caught up)                                                                                   |
| `?` (pending addition) | present      | **walker failure** with `pending-marker-resolved` — author must drop `?` to the present marker (`✓` for matrix) in the same diff |
| `~` (pending removal)  | present      | silent pass (pending — impl not yet caught up)                                                                                   |
| `~` (pending removal)  | absent       | **walker failure** with `pending-marker-resolved` — author must drop `~` to absent (blank for matrix) in the same diff           |

The walker continues to dispatch once and produce composite results; the failure
set excludes pending elements whose state matches the pending direction (`?` +
absent, `~` + present), includes elements whose pending state has resolved.

**The marker is self-cleaning** — modelled the same way as the per-annotation
`[tier?]` modifier above. The author who lands the impl that catches up to the
matrix cell (`{% include %}` added for `?`, removed for `~`) must drop the
marker to its resolved value in the same diff or the walker refuses on the
resolved-state failure. This forces co-incidence between _"impl caught up"_ and
_"marker resolved,"_ so the spec tree never carries stale pending markers past
the molecule's push gate.

**Concern token.** `pending-marker-resolved` — emitted by the walker when a
pending element's state has resolved against the pending direction. Target
variant depends on the walker emitting it (the matrix walker uses `MatrixCell`,
the surface walker uses `SurfaceElement`, etc. — each sweeping walker defines a
target variant naming the specific element that should be resolved). Routes to
remediation bead (not clarify) — the resolution is mechanical, not
judgment-requiring.

**Adoption convention.** Every sweeping walker added to loom MUST support `?`
and `~` in its input from day one. Retrofitting pending-marker support to
existing walkers (the matrix walker is the first case) is a
walker-implementation change tracked as an ordinary `loom loop` bead per the
planning session that surfaces the need.

##### Runners — per-language batched dispatch

[Acceptance](#runners--batched-dispatch).

**Runners, not verifiers, are the dispatch unit.** A runner executes one batch
of annotations in a single subprocess. Per-language batching avoids the "process
per test" cost that dominates wall-clock on non-trivial specs. `[system]`
remains single-scenario execution rather than a batch of distinct targets, but
equivalent invocations share one execution within a gate run (see _Execution_
below).

The dispatcher's job:

1. Collect all in-scope annotations (per _Verifier inputs_ + the scope flag's
   input set, intersected).
2. Group by which runner matches them.
3. For each runner with a batch template, build one command, spawn once, parse
   per-target verdicts from the output. `[system]` renders one scenario at a
   time and shares equivalent execution (see below).
4. Literal commands may execute only through an admitted provider, whether
   configured or built in. Unmatched/ambiguous provider contracts fail
   admission; equivalent `[system]` invocations still share within the run.

**Schema: `[runner.<tier>.<name>]` in `<workspace>/loom.toml`.** Each runner
declares how to recognise its annotations, how to format each target, how to
join into a batch, how to parse per-target results, and where to run from.

| Field     | Purpose                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `match`   | Regex (PCRE-compatible) over the annotation's target string. Annotations whose target matches are dispatched through this runner. Capture groups are referenced by `{capture_N}` in `target`. Optional — when omitted, this runner is the default for the tier.                                                                                                                                                                                                                                                                                                                                                  |
| `command` | Command-line template. `{filter}` or `{targets}` substitute the joined-target string; `{capture_N}` substitutes a regex capture from the matched target.                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `target`  | Per-target template applied to each matched annotation before joining. References `{name}` (full target) or `{capture_N}` (capture groups from `match`).                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `join`    | String inserted between formatted targets to build `{filter}` / `{targets}`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `parse`   | Named built-in parser (see below) that extracts per-target verdicts from the runner's stdout.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `cwd`     | Repo-relative directory to run the command from. Override the tier-default cwd.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `inputs`  | Command template for a provider's batched **description/input query**, with `{print_inputs}` locating `--print-inputs` in the verifier's own argv (without the placeholder it is appended). Checked definitions generate unit descriptions; an admitted provider may also consume an observation projection such as judge collect-mode paths. Neither is a separately maintained input/policy registry. A query is unnecessary when a built-in provider supplies the equivalent contract directly; omission never grants silent always-run admission. Missing contracts or invalid query results fail admission. |

**Built-in parsers** ship with loom — consumers add new runners that emit one of
these formats, rather than authoring custom parsers:

- `libtest-json` — Rust `cargo test`/`nextest` `--message-format` output: test
  events with `name` and `event` (`ok`, `failed`, `ignored`). Ignored tests
  produce a skipped result, not a pass.
- `junit-xml` — JUnit-XML reports (pytest, others). Parses XML under `testsuite`
  / `testsuites` roots; testcase identity is `classname.name` (or `name` without
  a classname). Direct `failure`/`error` children fail, `skipped` children skip,
  and otherwise the case passes. Failure takes precedence over skip. Malformed
  XML, missing names, duplicate identities, unsupported testcase children and
  unrecognized status/result attributes without an explicit failure/skip are
  dispatch errors.
- `nix-build-status` — `nix build`'s per-derivation success/failure output.
- `json-lines` — one checked per-target result per nonblank stdout line, using
  the schema in _Sandbox-capability results_ below. Legacy
  `{"target":"<name>","pass":bool,"evidence":"<msg>"}` remains supported.
  Diagnostics belong on stderr. Malformed records, duplicate targets/fields,
  missing required fields and conflicting outcomes are errors; records are
  parsed even when the producer exits 77. Shared annotation targets are sent
  once and their one result fans out to all owning criteria.
- `exit-code` — single per-runner verdict from the process exit code. Only
  useful for non-batched runners (one annotation per invocation).

Configured `[test]` parsers and named runners are used by both CLI verification
and the mint verifier walk. Every selected target must match a configured runner
(use a tier-default runner for the remainder). Command-only legacy test
configurations and toolchain discovery retain their aggregate exit-code/JSON
contract. When a known unstructured test summary reports skips, that aggregate
is recorded as skipped: it cannot establish which targets actually passed. The
discovered nextest runner requests `--status-level skip`: its summary counts
filtered-out tests as skipped, so explicit `PASS` receipts for every requested
target, without any matching skip/failure receipt, permit an aggregate pass.
Missing receipts and selected ignored tests remain conservatively skipped.
Failures in the summary or process status are not demoted by the presence of
skips. Use a per-target parser for precise mixed-batch evidence.

**Tier-default cwd.** A `[runner.<tier>]` block (no `.<name>` suffix) sets the
default cwd for unmatched annotations in that tier:

```toml
[runner.check]
cwd = "loom"  # default cwd for all [check] annotations
```

Resolution order when spawning a command:

1. The matched runner's `cwd` field, if set.
2. Else the tier's default `cwd` (`[runner.<tier>] cwd = "..."`), if set.
3. Else repo root (`.`).

**Loom-the-library ships defaults** for the common toolchains — nextest for
`[test]` if a `Cargo.toml` is detected, nix for `[system]` derivations, pytest
if a `pyproject.toml` is detected. Consumers extend or override in
`<workspace>/loom.toml`. **Loom-the-library has no privileged knowledge of any
consumer's layout** — defaults discover and validate native project definitions;
heuristics alone do not admit an execution-input contract.

**Runner-owned resolution, invocation, and input-query.** A runner that
`match`es an annotation **owns** that annotation end to end — loom never falls
back to parsing the annotation's argv for it:

- **Resolution.** The annotation resolves (integrity gate, direction 1) because
  a runner claims it, not because its first token is on PATH. The
  `tokens[0]`-on-PATH check is the fallback for annotations no runner matches.
  Existence or runner matching is only target resolution, not input admission. A
  configured or built-in provider must also supply the admitted execution
  contract; wrapping a command in `sh -c "…"` cannot avoid that requirement.
- **Invocation.** The `command` template is the single definition of how the
  verifier is spawned. Loom does not reconstruct the command by token-splitting
  the annotation; `--print-inputs` (and every other argument) lands where the
  template places it, so a `cargo run -p loom-walk -- {targets}` runner queries
  the walk, never `cargo`.
- **Input-query.** The provider supplies inputs from its tracked/checked
  execution definition, directly or through the runner's `inputs` query. For
  batched tiers (`[check]`, `[test]`, `[judge]`), one query returns the
  per-target map for the matched group. `[system]` may query distinct scenarios
  separately; repeated equivalent declarations reuse discovery within the
  session. Execution sharing does not change input admission or spec-contract
  inclusion.
- **Execution.** For the batched tiers (`[check]`, `[test]`, `[judge]`), matched
  annotations batch into one subprocess per runner (the dispatcher's step 3
  above); per-annotation spawn is only the unmatched fallback. `[system]` shares
  equivalent invocations that require execution once per gate run, matched or
  not, and retains one result and cache record per criterion. Equivalence
  includes the rendered command, resolved cwd, dispatch scope (`LOOM_FILES` /
  `LOOM_SPEC`), inherited environment, and scenario/fixture requirements. Target
  and matched-runner identity conservatively keep distinct scenarios separate
  even if their rendered commands coincide. An owning spec/criterion alone is
  not a distinct scenario. Failure, skip, producer-error and dispatch-error
  evidence fans out just like a pass; sharing never upgrades coverage. This
  dispatcher memo is run-local, not cross-run authority. Normal verification can
  consume [admissible prior evidence](evidence.md#cache-and-evidence-reuse)
  instead of dispatching a unit; it cannot simply replay raw output or cached
  status as current proof. [Exact-target diagnostics](gate.md#scope-flags)
  require fresh execution.

Literal argv semantics do not exempt a verifier from input admission.
Unregistered commands with no supported provider contract fail configuration
admission rather than acquiring guessed inputs or an implicit global scope.

###### Verifier inputs

[Acceptance](#verifier-inputs).

Selection correctness asks whether the right obligations are selected given
complete dependency information. Dependency completeness separately requires
both the **subject inventory** a verifier is responsible for and the
**execution-input closure** needed to examine it. A hermetic check can still
examine the wrong universe; neither Nix nor Rust types establish semantic
coverage.

Loom-owned checks consume immutable tracked provider inputs: domain membership,
content, existence, path resolution, and relevant configuration/tool identity.
Filesystem/environment access belongs at that boundary. Dependency reporting and
execution use the same accesses, not parallel registration/input lists.

External execution uses checked constructors. Construction starts with the
independent repository inventory, resolves subject/resource domains and native
targets, constructs the unit, and exposes its actual transitive execution graph
and source identities. Resource declarations supply execution itself; no second
smaller self-reported list may authorize selection or reuse. Cargo metadata
helps locate targets but omits many runtime/build/fixture inputs; Nix metadata
reflects declarations, not proof that the subjects/resources are complete.

Precision is relative to admitted responsibility and dependencies. Shared inputs
are normal. Independence fixtures identify changes that must not select
unrelated work; genuinely global checks are explicit and justified. File-count
thresholds and claims of semantic minimality do not replace provider conformance
and verifier-honesty review.

The input-query transport can expose repo-relative gitignore-style globs as a
projection of an admitted definition. A glob list alone does not establish
complete scope. Metadata discovery, actual observations, and execution resources
must agree; raw argv path guessing is not an admission mechanism.

**Input-query protocol.** The `--print-inputs` query is issued through the
verifier's runner / command template — **never by prepending the flag to the
command's first token.** The template decides where the flag lands, so a
`cargo run -p loom-walk -- foo` verifier is queried as the walk's own argument
(after the `--` boundary), not as an argument to `cargo`, and a
`sh -c "<script>"` verifier is queried by running the script, not by
token-scanning for a path. The
[provider description boundary](#provider-authoring-and-description-boundary)
extends this mechanism with generated typed JSON descriptions for one target or
an identified batch. Required target descriptions cannot be omitted, duplicated,
or attributed to another target. Keep discovery batched where execution batches;
scoping does not add a query subprocess per criterion or native test.

An already admitted provider may consume a narrower dependency-observation
projection, notably the judge collect-mode shapes below:

- **Single-target projection** — `{"inputs": ["glob", ...]}`.
- **Batch projection** — `{"inputs": {"<target>": ["glob", ...], ...}}`.

These projections supplement the provider's checked contract. A bare list of
paths or globs is not a replacement for a generated unit description or direct
checked provider definition and cannot establish complete scope on its own.

**Target resolution.** A `[judge]` target is located by selector-stripping +
spec-relative resolution: a `#fn` / `::fn` / `::attr` selector is stripped
before the on-disk lookup, and a relative path is joined against the
annotation's spec-file directory (not the repo root), matching the markdown
renderer's relative-link resolution. The integrity gate and the input resolver
share **one helper** for this, so the existence check and the collect-mode
invocation cannot disagree about where the judge script lives. This is the
deterministic resolution of a target that genuinely _is_ a path. A `[check]` /
`[system]` target is _not_ path-resolved this way: it resolves by **runner
match** — the matching runner owns the annotation end to end (per _Runners_) —
or, when no runner matches, by the `tokens[0]`-on-PATH fallback. Its inputs come
from the matched runner's template (per _Input-query protocol_), never from
scanning its argv to guess which token is a file.

**Judge collect mode.** A judge script reports per-function inputs by running
the function in a **collect mode** rather than evaluating it:
`<script> --print-inputs <fn>` defines `judge_files` to _record_ its path
arguments and `judge_criterion` (and any LLM call) as a no-op, runs `<fn>`, and
emits the recorded paths as `{"inputs": [...]}`. Invoked with **no** `<fn>` the
script emits the batch map for every rubric it defines
(`{"inputs": {"<fn>": [paths], ...}}`) in a single spawn, so the gate learns one
script's whole judge set at once. The `judge_files` calls a rubric already makes
are therefore the **single source of truth** for that judge's inputs —
per-function, with no separate header to maintain or drift. This requires judge
scripts to be executable with the loom judge-harness preamble (which supplies
`judge_files` / `judge_criterion`); a judge whose collect mode errors or emits a
malformed inputs document is a loud finding, not a silent fallback (see
_Inputs-protocol error_).

**Spec-contract inclusion.** Applicable package contracts, owning criteria, and
strategy participate automatically in affectedness and evidence validation.
Changing them cannot leave old authorization intact. Their inclusion is
additional to the verifier's own dependency closure, never a replacement for a
missing input contract.

**Strict admission.** Every executable verifier has an exact target, subject
inventory, supported discovery contract, execution requirements, and result
mapping. Missing contracts, ambiguous targets, malformed metadata, or discovery
failures block trusted planning. An explicit justified broad/global contract is
valid; silently converting failed discovery into always-run is not. A valid cold
tracked provider may execute to collect observations. Absence of earlier traces
does not mean an empty dependency set or a malformed contract.

**Inputs-protocol error.** A provider query that fails or emits malformed
metadata produces `inputs-protocol-error`; there is no heuristic or alternate
provider fallback. Missing contract admission is likewise a configuration
failure, even when a literal command exists on PATH. No command is probed by
guessing unsupported flags: configured or built-in providers own their queries.
An empty query projection is accepted only when the admitted definition really
has no corresponding file inputs; it cannot erase membership, configuration,
tool, or contract dependencies. Pending annotations retain their separate
pending policy rather than requiring a not-yet-existing verifier contract.

**Repo-agnostic.** The `--print-inputs` convention works for any script or
binary in any language, and the `[runner.<tier>] inputs_for_test` config knob
handles non-default test frameworks — loom-the-library imposes no layout of its
own.

Spec annotations stay **clean** — `[tier](target)` and nothing else. No inline
input metadata, HTML-comment companions, or in-script `# loom-inputs:` list is
required. Execution-owned dependencies are distinct from spec-owned strategy in
`loom-verify` blocks; neither is a parallel manually maintained input list.

##### Verifier-runner contract

[Acceptance](#dispatch--json-verdict-and-exit-code-fallback).

Every verifier — whether `[check]` command, `[system]` command, or the runner
invoked by batched dispatch — is a subprocess that conforms to:

- **Input:** env vars (`LOOM_FILES=<paths>` for `--files` runs,
  `LOOM_SPEC=<label>`, etc.) plus argv from the annotation's command string.
- **Output:** a JSON line on stdout matching the typed-verdict shape —
  `{"pass": bool, "evidence": "<message>"}`. Batched runners emit one such line
  per target via the `json-lines` parser, or use one of the other built-in
  parsers (`libtest-json`, `junit-xml`, `nix-build-status`).
- **Exit code:** `0` for all-pass, `77` for skips without failures, a nonzero
  code other than `77` for any failure (`1` normally, `2` for dispatch errors
  such as unknown verifiers or invalid invocation). Failures dominate skips;
  skips dominate passes. JSON-lines process status and result aggregation agree;
  a failed process cannot become successful by claiming passing records.

This works for any language. The contract is process-shaped, not
language-shaped.

**Exit code 2 is a fail at the push gate.** Dispatch errors — a spec annotation
referencing a walk that doesn't exist, a binary that isn't on PATH, a command
with a missing flag — produce exit code `2`. The gate treats this as a hard fail
(not a skip): the verifier the spec is claiming exists, and the gate cannot
confirm it did anything. The [publication gate](gate.md#gate-success-receipt)
refuses on any verifier exit ≠ 0, including dispatch errors. This closes the
failure mode where a spec asserts `[check](cargo run -p loom-walk -- foo_bar)`
for a walk `foo_bar` that nobody implemented — exit 2 → push refused → the
missing implementation surfaces immediately.

**Fallback for non-conforming verifiers.** Bare `grep -q`, `cargo test`,
`nix build`, and similar shells that don't emit a JSON verdict line still
satisfy the contract via their exit code alone: the dispatcher interprets exit 0
as `pass=true` (stdout surfaced as evidence), exit 77 as skipped, and other
nonzero exits as failures. Skipped execution is not an observed pass, and exit
77 alone does not authorize worker acceptance. Unstructured, ignored-test and
legacy skips without checked platform/capability metadata remain blocking. A
JSON claim cannot override a failed process; conflicting or duplicate verdicts
fail closed. Status-cache verdicts remain `pass`, `fail`, and `skipped`;
verifier evidence version 2 invalidates older criterion rows once while
retaining unrelated cache state.

Verifiers that emit a JSON line are preferred — the explicit evidence string
clicks straight to the violation site — but the exit-code fallback keeps simple
presence/absence checks viable without wrapping each one in a Rust walk.

##### Sandbox-capability results

[Acceptance](#mechanisms).

The worker-stage policy is distinct from later host testing. Workers execute
locally in their sandbox; no SSH access, host-verifier bridge, remote per-bead
receipts, host provisioning or live Darwin evidence is required. Required host
testing follows [stage applicability](gate.md#deterministic-verify-lanes);
worker acceptance never substitutes for that coverage.

A modern JSON-lines producer emits exactly one record per requested target:

```json
{"target":"linux-check","outcome":"passed","evidence":"executed assertions","execution":{"platform":"x86_64-linux","platforms":["x86_64-linux"],"capabilities":[]}}
{"target":"darwin-check","outcome":"skipped","evidence":"not executed","execution":{"platform":"x86_64-linux","platforms":["aarch64-darwin","x86_64-darwin"],"capabilities":[]},"skip_reason":{"kind":"foreign-platform","reason":"macOS APIs unavailable on Linux"}}
{"target":"vm-check","outcome":"skipped","evidence":"not executed","execution":{"platform":"x86_64-linux","platforms":["x86_64-linux"],"capabilities":["kvm"]},"skip_reason":{"kind":"missing-capability","capability":"kvm","reason":"sandbox has no /dev/kvm"}}
```

`outcome` is `passed`, `failed` or `skipped`. `target` and `evidence` are
required; modern outcomes also require `execution`. Its `platform` is the actual
canonical `<architecture>-<OS>` platform (`darwin` for macOS); `platforms`
declares applicability (empty means all platforms), and `capabilities` declares
local runtime prerequisites. Platform/capability tokens are nonempty ASCII
letters, digits, `-`, `_` or `.`; duplicate requirements are invalid.
`skip_reason` is required for a modern skip and forbidden on passes/failures.
Its `reason` is nonblank. Unknown fields/variants, missing or unrequested
targets, duplicates, and contradictions are rejected. Optional fields are
omitted rather than null; present null values are malformed. Legacy flags may
accompany an explicit outcome only when consistent with it. Without an explicit
outcome, legacy `pass:false` is a failure unless `skipped:true` is present;
evidence text is never classified. Legacy skips without metadata cannot gain
policy permission.

Wrix-compatible configuration (repeat the same runner fields under
`[runner.system.wrix]` for system-tier targets):

```toml
[runner.check.wrix]
match = '^verify:(.+)$'
command = 'nix run .#verify -- {targets}'
target = '{capture_1}'
join = ' '
parse = 'json-lines'
cwd = '.'
skip_policy = 'sandbox-capability'
skip_capabilities = ['container-runtime', 'kvm']
```

`skip_policy` defaults to `deny`; `sandbox-capability` requires `json-lines`.
`skip_capabilities` is an explicit allowlist, empty by default. A
foreign-platform skip is permitted only when the reported actual platform
matches Loom's execution platform and is outside declared applicability. A
missing-capability skip is permitted only on an applicable platform, with the
unavailable capability both declared by the target and allowlisted by the
runner. The producer owns honest local capability preflight: check availability
before executing, preserve the actual execution failure as failed, and never
retrofit requirements to excuse an assertion failure, broken invocation,
arbitrary missing resource or other unexpected skip. Loom checks
declarations/policy consistency, not the truth of an external producer's claim;
this is not remote verification evidence.

A tier command containing permitted skips exits 77, including in a worker. Only
worker `loom gate verify` (`LOOM_INSIDE=1`) may accept an otherwise clean run
with those permitted skips and exit 0; its diagnostics say coverage remains
unverified. A failure, dispatch error, unpermitted skip or aggregation conflict
still blocks. Outside the worker, `gate verify` retains exit 77 for permitted
skips. Skipped targets stay `skipped`, never `pass`, in cache and reports;
structured execution metadata, reason and policy decision survive in the cache
`evidence` JSON and `.loom/logs/gate/verifier-results.jsonl`. Worker acceptance
writes an `accepted-with-skips` lifecycle log without hook coverage; it cannot
construct `VerifiedScope`, validate `GateSuccess` or authorize a push marker.
Marker schema 4 rejects older markers, and host hooks do not consume worker
acceptance/cache as proof of passed system coverage. The synthetic native CLI
fixture uses the same seam with `bash producer.sh {targets}`; it proves parser
and runner integration, not the consumer's system tests.

##### `--files` scope handling

[Acceptance](#admitted-inputs-and-selection).

The planner selects eligible obligations using admitted dependencies before
batching or system dispatch. This applies across tiers rather than delegating
system affectedness to an arbitrary shell command. Native suite/property
boundaries follow [Verify](#selective-property-campaigns).

Selection scope is not analysis scope: `LOOM_FILES` identifies requested changes
but does not authorize omitting unchanged subjects needed by a cross-file check.
A verifier narrows execution to changed files only when its admitted semantics
permit that narrowing. A known-unaffected unit is excluded; a required unit runs
or consumes admissible evidence under the same rules as other scopes.

##### Test-tier silent-zero-match

[Acceptance](#dispatch--per-tier-process-model).

`cargo test -- some_name` and equivalents in other runners exit 0 silently when
no test matches the filter. The gate sniffs known runners (`cargo test`,
`cargo nextest`, `pytest`) and post-processes output to detect zero-match cases,
failing the run with a clear error. Consumers using unrecognised runners must
ensure their runner fails on zero-match.

#### Integrity gate

[Acceptance](#integrity-gate--four-directions).

The deterministic gate that verifies the annotations themselves resolve. Runs as
part of `loom gate check`. Four directions:

1. **Forward — every annotation's target is valid for its tier.**
   - `[check](target)` and `[system](target)`: the target resolves via a
     matching runner (`[runner.<tier>.<name>] match`), or — when no runner
     claims it — its first token resolves on PATH or as a file in the repo
     (best-effort; dynamic commands may resolve only at runtime). Runner-match
     is the primary path; the `tokens[0]` check is the unregistered-command
     fallback.
   - `[test](path)`: the path resolves to a `#[test]` / `#[tokio::test]` /
     proptest function (or language equivalent) in the consumer's workspace, via
     the consumer's toolchain metadata.
   - `[judge](path)`: the path resolves to a file on disk.

   The pending modifier `?` (see [_Pending modifier_](#pending-modifier) above)
   flips the per-annotation outcome. For `[check?]` and `[system?]`, pending
   resolution uses the full dispatcher command, not the first-token / file
   lookup used by the non-pending forms above: a spawn failure or non-zero exit
   remains pending, while exit 0 emits an `UnneededPendingMarker`. `[test?]` and
   `[judge?]` retain their tier-specific target-resolution checks. The finding
   names the spec, line, and target so the implementer can drop the `?` in the
   same diff that lands the verifier.

2. **Stub-pointing — annotations whose verifier body invokes the `_pending_stub`
   sigil are flagged** (`StubTestFunction`). A stub means the criterion has no
   real evidence; the deterministic gate flags it without waiting for
   `loom gate review`'s verifier-honesty rubric. The pending modifier suppresses
   `StubTestFunction` the same way it suppresses `UnresolvedAnnotation`; once
   the test body becomes non-stub the modifier triggers `UnneededPendingMarker`
   for that annotation.

3. **Atomic acceptance — each criterion carries exactly one annotation.** Two
   annotations on one criterion is a flag (ambiguous pass/fail when one passes
   and the other fails). N→1 sharing is allowed (multiple criteria pointing at
   the same verifier). Atomic acceptance is structural and **not** suppressible
   by `?` — having two annotations on one criterion is wrong regardless of
   either's resolution state.

4. **Input-contract admission and protocol honesty.** Executable verifiers
   require admitted providers; target existence alone cannot establish a scope.
   Missing/ambiguous contracts and discovery failures block trusted success. A
   declared query that fails or emits malformed metadata produces
   `inputs-protocol-error`. No silent always-run or heuristic fallback is
   permitted. The pending modifier retains its separate treatment of absent
   verifiers and suppresses `inputs-protocol-error` during that pending window.

Failure output (one per finding):

- `<spec>:<line>: annotation [tier](<target>) — does not resolve`
- `<spec>:<line>: criterion carries N annotations, expected 1`
- `<spec>:<line>: annotation [tier](<target>) points at stub function`
- `<spec>:<line>: annotation [tier?](<target>) is now resolved — drop the ? marker`
- `<spec>:<line>: annotation [tier](<target>) — input-query errored / emitted a malformed inputs document`

**Integrity findings at the push gate are recoverable up to the molecule's
iteration cap.** When deterministic push verification over
`origin/<integration-branch>..HEAD` produces one or more `UnresolvedAnnotation`,
`StubTestFunction`, `UnneededPendingMarker`, or `inputs-protocol-error` findings
within the actual push range, the verdict gate normalizes each into a typed
`Finding` per the mapping in _Findings and Minting — Concern tokens and target
variants_ and merges them into the molecule's deferred remediation set (per
_Deferred remediation processing_). The findings coalesce into one remediation
batch per lead-spec / concern family, carrying all integrity findings the audit
emitted for it. The push is refused for this iteration, the iteration counter is
incremented, `loom gate mint -m/--molecule` promotes the batch, and the outer
loop re-enters so the worker can address it.

**Cap-exhausted fallback.** The recovery branch is bounded by the molecule's
iteration cap. When the counter exhausts, the verdict gate falls back to the
terminal escalation: `loom:clarify` on the molecule's epic with **one composed
`## Options — …` block** per the _Options Format Contract_. The composition rule
is mechanical:

- For each integrity finding kind present in the molecule's findings (in the
  order they appear below — `UnresolvedAnnotation`, `StubTestFunction`,
  `UnneededPendingMarker`), emit one `### Option N` entry drawn from the
  **primary (Option 1) of that kind's per-kind auto-options template** below,
  scoped to the affected findings (e.g. _"Option 1 — Drop the `?` markers at
  specs/templates/tests.md at the reported annotation locations"_).
- Close the block with one final `### Option N` for _"Mixed resolution via
  `loom inbox chat`"_ — the escape hatch when the operator needs different
  resolutions across findings or wants options beyond each kind's primary.

This preserves the Options-Format-Contract invariant of one block per clarify
bead while keeping per-kind resolution paths visible to the operator.

**Worker authority on the recovery branch.** Findings are not classified as
self-fixable in the driver; the worker is the authority on whether one turn can
resolve every finding in the batch. A worker that cannot resolve the batch emits
`LOOM_CLARIFY` from its own dispatch, which routes through the standard per-bead
clarify path — the iteration cap is the backstop for both "worker keeps failing
on the same finding" and "findings are intrinsically clarify-shaped."

**Per-kind auto-options templates.** The templates below are the building blocks
the composition draws from. Two consumption sites:

- **Recovery branch (cap not exhausted).** The worker's remediation batch
  description embeds the kind-appropriate template alongside each finding as a
  suggested mechanical resolution.
- **Cap-exhausted fallback.** The gate composes one primary option per present
  kind from these templates per the rule above.

**Auto-generated options for `UnresolvedAnnotation`.** The gate has enough
information (target string, tier, spec location) to draft options for the human:

- _Option 1_ — Implement the missing verifier (walk / test / judge / system
  check) at the expected path.
- _Option 2_ — Retarget the annotation to an existing verifier (gate lists
  nearest matches by name).
- _Option 3_ — Mark the annotation pending with `?` if the verifier is
  intentionally deferred to a follow-on bead — the integrity gate will then
  silently accept it until the implementing diff drops the `?` and the target
  resolves in the same commit.
- _Option 4_ — Remove the criterion at `<spec>:<line>` if it's superseded or out
  of scope.

**Auto-generated options for `StubTestFunction`.** Similar shape:

- _Option 1_ — Implement the test body, replacing the `_pending_stub` sigil.
- _Option 2_ — Retarget the annotation to a non-stub verifier.
- _Option 3_ — Mark the annotation pending with `?` if the implementation is
  intentionally deferred (same self-cleaning semantics as for unresolved
  targets).
- _Option 4_ — Remove the criterion if the work isn't planned.

**Auto-generated options for `UnneededPendingMarker`.** The marker is stale; the
implementation has caught up to the claim:

- _Option 1_ — Drop the `?` from `[tier?](<target>)` at `<spec>:<line>` so the
  annotation reads `[tier](<target>)`. This is the expected resolution and
  almost always the right one.
- _Option 2_ — If the resolution is incidental (the target name collides with an
  unrelated symbol now visible in the workspace), retarget the annotation to the
  actual intended verifier and keep `?` until _that_ one resolves.

The integrity gate is itself a `[check]`-tier verifier (its own spec criterion
annotates back to its implementation), so every `loom gate check` run includes a
self-test of the gate's resolution logic.

### Functional

14. **Verifier-driven status; no checkboxes in spec markdown.** Success Criteria
    bullets carry their `[check]` / `[test]` / `[system]` / `[judge]` annotation
    but **no `[ ]` / `[x]` prefix**. Status is a property of running the
    verifier against the current code-spec pair, not a value stored in the spec.
    `loom gate verify` enumerates every annotation in scope and reports
    per-criterion `pass | fail | skipped` from current admitted executions or
    reusable evidence. A cached status scalar is not admission; past passes do
    not grant immunity from current obligations and policy. This rules out the
    failure class where a checkbox is `[x]` while the verifier points to a stub,
    or where production behaviour diverges from the unit-tested function the
    verifier exercises. Each request re-evaluates applicability and evidence,
    executing whenever reuse is not admissible.

#### loom-walk

[Acceptance](#verifier-inputs).

- Walk dispatch: `loom-walk <name>` invokes the named walk; an unknown name
  exits non-zero with a clear error naming the available walks
- Walk selection honors admitted affectedness. `LOOM_FILES` may narrow execution
  only where the declared semantics permit it; a cross-file walk still reads
  unchanged subjects needed for analysis.
- Output conforms to the verifier-runner contract: one JSON line on stdout
  (`{"pass": bool, "evidence": "<path>:<line> <rule>"}`), exit code mirrors
  `pass`
- Per-walk fixtures: each walk has a `#[test]` exercising both pass and fail
  cases against synthetic source under `tempfile::tempdir`

#### loom-gate

- Annotation parser: walks canonical package acceptance documents, extracts
  `[tier](target)` annotations, returns typed `Annotation` records (tier,
  target, source spec, line)
- Per-tier dispatch, including system equivalent-execution sharing and criterion
  evidence fan-out, follows
  [Verify — Runners](#runners--per-language-batched-dispatch)
- Toolchain detection: `Cargo.toml` at root → cargo nextest runner template;
  `pyproject.toml` → pytest; `go.mod` → go test
- `<workspace>/loom.toml` loading: `[runner.<tier>.<name>]` tables parse into
  per-tier runners with `match`/`command`/ `target`/`join`/`parse`/`cwd` fields;
  missing file falls back to detected defaults

### Functional

- Integrity gate forward direction: every annotation's target is valid for its
  tier (resolves on PATH for `[check]` / `[system]`; resolves to a `#[test]`
  function via cargo metadata for `[test]`; resolves to a file on disk for
  `[judge]`)
  - Integrity gate atomic acceptance: each criterion carries exactly one
    annotation
  - Integrity gate self-test: its own criterion in `tests.md` annotates back to
    its implementation
  - Native selection uses checked source/resource membership and execution
    dependencies, including manifests, fixtures, and property campaign inputs.
    Cargo metadata contributes target/package edges; a transitive `.rs` list
    alone is not complete admission or precise per-property impact.
  - Test-tier silent-zero-match sniffing: cargo / nextest / pytest stdout
    post-processed to detect zero-match cases and fail loud

## Out of Scope

- Publication authorization belongs to Gate; durable result trust belongs to
  Evidence. A separate generic incremental engine, speculative scheduling,
  precise arbitrary Rust function-impact analysis, and additional
  general-purpose trusted non-Nix providers are outside this pilot.

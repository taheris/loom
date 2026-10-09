# Gate composition and publication

Composes deterministic verification and semantic review for explicit scopes, and
admits whole-scope publication receipts and marker handoffs.

## Problem Statement

Passing individual checks does not establish that the current publishable state
has complete verification and review. Gate composes those obligations and admits
publication only with matching, current evidence.

## Architecture

Verify plans and executes checks; Evidence admits their results; Gate composes
whole-scope review and publication authority. Related owners:
[verify](verify.md), [evidence](evidence.md), [findings](findings.md),
[loop](loop.md), [pre-commit](pre-commit.md), [protocol](protocol.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Components

`loom gate` is one command tree spanning deterministic verification, LLM review,
finding minting, and marker validation across plan, worker, integration,
stabilization, push, and standing stages. This spec owns the gate rubric,
invariants, stages, typed success receipt, and trust marker;
[Loop](loop.md#verdict-gate) owns the worker and per-bead execution mechanics
that consume those results. Only `loom gate mint` mutates bd state; verification
and review remain inspection paths.

#### Publication-attempt handoff

[Acceptance](#execution-and-admissible-evidence).

Driver verification, review, and the actual pre-push handoff form one
publication attempt. Fresh evidence can complete that same attempt under the
ordinary freshness and invalidation rules, including when contradictory history
prohibits later result reuse. Consuming that evidence at the hook handoff is not
a second verification-result cache hit. On a later attempt, a reuse-disabled
identity must execute again even if content/range/config fingerprints match.
Receipts and markers cannot turn same-attempt evidence into authorization for a
later attempt.

The attempt is represented by a live, host-owned single-push handle with an
OS-backed lifetime. Verification, review, and the actual push consume that
handle in the admitted local context; a serialized identifier, surviving marker,
PID string, timestamp, or cleanup callback alone cannot establish its validity.
Loss of the owning host process ends the attempt without relying on cleanup.
Failed or interrupted push also ends it when that process survives. A live
handle is necessary for this handoff, not sufficient authorization: current
coverage, evidence admission, and freshness still apply. This introduces neither
a daemon nor driver-only review for marker-free operator pushes or rehearsals.

A failed or interrupted `git push` ends the publication attempt, including
process loss without a recorded outcome. A retry starts a new attempt and
re-enters Gate planning and admission; unchanged fingerprints or surviving
receipts cannot resume the ended attempt. Reuse-disabled identities execute
afresh, and freshness-sensitive claims receive fresh observations. Independent
admissible unit results and build/image caches remain reusable. Failure or
interruption does not prove that the remote stayed unchanged: the retry resolves
actual current Git publication state. This boundary does not introduce automatic
retries or change existing retry budgets.

[Loop](loop.md#verdict-gate) supplies workflow context; this spec owns its
admission. [Pre-commit](pre-commit.md#plumbing-ownership-split) prevents outer
hook shortcuts from bypassing that admission. Attempt representation belongs to
the caller protocol, not inference solely from a SHA or transaction stamp.

### Coverage reductions

[Acceptance](#coverage-reductions-1).

Review explicitly scrutinizes exemptions and narrowed checker responsibilities.
Removed obligations must remain covered by another owner or be justified by a
spec change; passing the remaining checks is not justification for checking
less. Dependency-precision improvements that retain coverage need no separate
approval process. Use the existing conformance/verifier-honesty review,
findings, and routing, not a new gate or mandatory human signoff for every
optimization.

## Success Criteria

### Planning, coverage, and strategy

<!-- prettier-ignore -->
- Trust-bearing requests supply snapshot, scope, and stage; the production gate
  derives required obligations from current contracts and policy rather than
  accepting a caller-picked test list as complete coverage. Exact-target
  diagnostics remain non-authorizing. [system?](nix run .#test-quint -- request-boundary)

### Admitted inputs and selection

- Empty finite scopes, invalid diffs, exact targets, and whole-tree requests
  retain distinct meanings throughout planning and authorization.
  [test?](quint_scope_meaning_survives_the_pipeline)

### Execution and admissible evidence

<!-- prettier-ignore -->
- Unchanged admissible unit evidence can survive unrelated edits, but cannot
  carry old whole-scope authorization into a new request. Changes after cache
  validation invalidate affected evidence or authorization before publication.
  [system?](nix run .#test-quint -- cache-authorization)

<!-- prettier-ignore -->
- Publication rejects mismatched content, scope, policy, configuration, hook
  coverage, or review evidence, including after retries, rebase, or interrupted
  execution. Worker feedback and partial inspections cannot authorize a push.
  [system?](nix run .#test-quint -- publication-boundary)

<!-- prettier-ignore -->
- Fresh evidence may flow through verification, review, and pre-push handoff in
  one publication attempt without redundant execution. Failed or interrupted
  pushes end the attempt; retries re-enter admission with fresh observations and
  fresh execution of reuse-disabled identities, while independent admissible
  results remain reusable. Old receipts or markers cannot resume an ended
  attempt even at unchanged fingerprints, and an unknown push outcome cannot
  establish unchanged remote state. Same-attempt handoff never overrides
  invalidation or freshness. [system?](nix run .#test-quint -- publication-attempt-handoff)

<!-- prettier-ignore -->
- The production publication handoff requires a live host-owned single-push
  handle with OS-backed lifetime. Actual owner-process loss and failed or killed
  push with a surviving owner invalidate the attempt even when old marker files
  remain. Files, identifiers, or claimed liveness cannot revive it; a valid live
  handle still requires current Gate admission and preserves existing
  marker-free push and rehearsal policy. [system?](nix run .#test-quint -- publication-attempt-handoff)

The gate packages use the same annotation taxonomy as consumers. Verify owns
[the self-hosting integrity checks](verify.md#integrity-gate--four-directions);
this acceptance document owns Gate composition and authorization.

### Gate evidence and marker coverage

- Every `loom gate` invocation that runs work writes a JSONL gate log under
  `.loom/logs/gate/` and emits `driver_event` records for `gate_run_start`,
  `gate_run_scope`, per-lane progress, and `gate_run_end`; a start without an
  end is treated as incomplete evidence, not success
  [test](gate_invocations_emit_jsonl_lifecycle_events) Gate lifecycle events use
  the canonical event schema owned by
  [Events — Driver Events](events.md#driver-events).

- `VerifiedScope` is constructible only from a successful deterministic
  `GateRun`; `ReviewedScope` is constructible only from a successful review run;
  `GateSuccess` is constructible only from matching `VerifiedScope`,
  `ReviewedScope`, pre-push hook coverage, and current tree/config/range
  fingerprints
  [test](gate_success_constructor_requires_typed_scope_and_coverage_evidence)

- Publication receipt admission consumes Loop's current molecule-clean
  reconciliation, rejecting known unaccepted workflow closure after restart.
  Beads progress, cleanup eligibility/disposal, and historical labels cannot
  substitute for that reconciliation or the receipt's required evidence;
  ordinary historical closed work does not acquire a retrospective receipt
  requirement.
  [test?](gate_receipt_requires_current_workflow_completion_admission)

- `loom gate review --diff <range>` consumes the latest matching `VerifiedScope`
  for the same resolved content/scope when producing push-eligible evidence; no
  production gate path passes or accepts `--verify-exit`
  [test](gate_review_consumes_verified_scope_not_verify_exit_scalar)

- A driver-minted marker short-circuits a wrapped pre-push hook only when it
  proves the same tree OID / clean porcelain, same `.pre-commit-config.yaml`
  digest, same resolved push range, a successful pre-push `GateRun` containing
  that hook id/entry as passed, and successful `VerifiedScope` + `ReviewedScope`
  for the same range; otherwise `pre-push-checks` falls through
  [test](marker_short_circuit_requires_hook_coverage_for_same_tree_config_and_range)

- Worker acceptance with skipped coverage cannot validate a push marker
  [test](worker_acceptance_with_skips_cannot_validate_existing_push_marker)

- Markers predating strict producer-result policy are rejected
  [test](markers_from_before_strict_result_policy_are_rejected)

### Commands surface — explicit scopes and status

- `review`, `judge`, `rubric`, and `audit` inspections never publish, mint
  markers, mutate Beads, or advance recovery iterations, independently of
  internal environment flags and reviewer verdicts
  [test](review_commands_are_read_only_without_internal_environment_flags)

- Review `--bead` supplies intent metadata without acquiring a work-root lock
  [test](review_bead_context_takes_no_work_root_lock)

- Judge `--target` loads only the exact selected annotation's source and pins
  its selector, excluding sibling targets and the rubric walk
  [test](judge_target_selects_exact_annotation_and_preserves_selector)

- Exact judge targets can span shared declarations in multiple specs; a context
  label never filters those declarations
  [test](judge_target_crosses_context_labels_and_deduplicates_shared_declarations)

- Unknown, partial, glob-shaped, and wrong-tier judge targets fail before
  backend dispatch
  [test](judge_target_rejects_unknown_partial_and_wrong_tier_matches_before_dispatch)

- CLI judge `--files` preserves workspace-wide judge selection rather than
  applying test-input intersection
  [test](judge_files_does_not_apply_test_input_intersection)

- Partial inspections cannot consume full verified scope as push authorization
  [test](partial_inspections_cannot_consume_full_verified_scope)

- A full diff review consuming matching verified scope emits completed review
  evidence and a parseable stdout handoff, without publishing itself
  [test](only_full_matching_verified_review_produces_push_evidence)

- Stale verified fingerprints fail before the reviewer is dispatched
  [test](full_review_rejects_stale_verified_fingerprint_before_dispatch)

- Concerns, malformed finding/marker pairs, blocked reviews, and missing markers
  cannot produce completed push-gate review evidence
  [test](rejected_review_walks_never_produce_completed_push_evidence)

- Production review stdout handoffs use shared message decoding and contextual
  finding resolution before scope-evidence admission; valid pretty-printed
  payloads work, malformed or wrong-phase messages cannot grant review
  authority, and live rendering/prompt/tool text cannot become result records.
  [test?](review_handoff_uses_shared_messages_before_scope_evidence_admission)

- Bare `loom gate` (no subcommand) prints `loom gate --help` — identical output
  to `loom gate --help`. No verifier runs, no cache read, no bd writes
  [test](bare_loom_gate_prints_subcommand_help)

- Bare `loom gate verify` prints the verify subcommand help and runs no
  verifier, project hook, cache lookup, bd write, or marker check
  [test](bare_loom_gate_verify_prints_help_and_runs_nothing)

- `loom gate verify` rejects a positional selector; exact target selection uses
  `--target <annotation target>` [test](gate_verify_rejects_positional_selector)

- `--target` is mutually exclusive with `--files`, `--diff`, and `--tree`; zero
  matches fail loudly; cross-tier matches under `verify --target` fail and
  suggest a tier subcommand; multiple criteria sharing the same target inside
  one tier are accepted [test](gate_target_exact_match_and_ambiguity_rules)

<!-- prettier-ignore -->
- Explicit `--target` requests require fresh verifier execution rather than
  verification-result reuse at the gate or provider boundary. Build and image
  artifacts remain reusable; a cached verification result cannot be reported as
  a fresh execution. [system?](nix run .#test-quint -- target-fresh-execution)

- `loom gate verify --diff <base>..<head>` runs the project pre-commit lane via
  prek for the resolved range before the spec annotation lane; if a hook
  modifies the working tree the run exits non-zero with a tree-modified-by-hook
  failure
  [test](post_integration_verify_runs_project_precommit_and_affected_check_test)

- A nested `loom gate verify --files` invoked under a parent `verify --diff`
  records a skipped gate event with reason `parent-diff-gate` and exits 0;
  correctness does not depend on the project's hook id
  [test](nested_verify_files_under_parent_diff_gate_records_skip)

<!-- prettier-ignore -->
- Finite `verify --files` and `verify --diff` select affected `[check]`,
  `[test]`, and `[system]` annotations by default. Stage-based exclusions
  require a criterion-local spec declaration, such as publication-only
  applicability; absence never implicitly excludes the system tier. An eligible
  criterion still requires its verifier when another criterion sharing that
  verifier is deferred; local deferral neither leaks to siblings nor overrides
  execution sharing. Stage-only edits change planning under the current policy
  even when criterion IDs and verifier targets remain unchanged. `verify --tree`
  and mandatory publication retain full required coverage. All callers use the
  same planning and dispatch path, with no `LOOM_VERIFY_TIERS` override or
  separate conformance hook. [system?](nix run .#test-quint -- finite-verify-stages)

<!-- prettier-ignore -->
- Verification applicability is selected automatically through existing
  entrypoints: standalone finite verification and ordinary worker/integration
  feedback use feedback applicability; driver publication and pre-push paths,
  including manual pushes without a marker and explicit hook rehearsals, request
  publication coverage. No public stage selector is required or introduced.
  Missing or invalid required publication context cannot downgrade that coverage
  to feedback. Full-tree coverage, worker capability limits, and the separation
  between inspection results and publication authority remain intact.
  [system?](nix run .#test-quint -- finite-verify-stages)

- `loom gate status --diff <range>` / `--files <paths...>` / `--tree` reads
  criterion evidence from `.loom/cache.db` and prints the report per
  [Evidence — Status cache](evidence.md#status-cache-1); status without an
  explicit scope prints help and runs no cache lookup [test](loom_gate_status_requires_explicit_scope)

- `loom gate status` is `refused_inside_loom() == false`; running under
  `LOOM_INSIDE=1` is allowed because the cache read is local and read-only
  [test](loom_gate_status_is_allowed_under_loom_inside_env)

### Scope handling

- Resolved selection distinguishes explicit files (including empty), whole tree,
  and exact targets without mutable combinations of scope flags
  [test](resolved_scope_distinguishes_empty_tree_target_and_file_provenance)

- A valid empty diff does not execute whole-tree deterministic verifiers
  [test](empty_diff_never_runs_whole_tree_verifiers)

- Live-workspace scope for a `[test](crate::module::test)` annotation includes
  the owning crate's files plus its transitive dependency files
  [test](live_workspace_scope_includes_own_files_and_transitive_dep_files)

### Verdict gate

- Worker/review guidance reserves semantic blocking for dead ends whose reason
  explains why safe options cannot be framed. Decision-worthy Loop/Todo work
  reports dedicated decision references independently of its terminal outcome;
  inspection-only review reports clarify-route findings with Options evidence,
  never direct decision-bead mutation.
  [judge](../tests/judges/agent-output-routing.md)

- Review's primary concern is live-path coverage: relevant `[check]` / `[test]`
  / `[system]` verifiers on the reviewed range must exercise the live path (same
  binary, same argv shape, same env). All-mock verifier sets raise a
  `LOOM_CONCERN` [judge](../tests/judges/loom.sh#judge_live_path_coverage)

- Review raises a `LOOM_CONCERN` on mocks that stand in for the very thing the
  test claims to test (e.g. mocking the agent backend in an agent-integration
  test) [judge](../tests/judges/loom.sh#judge_mock_discipline)

- Review's secondary concerns are scope appropriateness and `[judge]` rubric
  satisfaction [test](review_renders_review_context_fields)

- Production finite-review context includes current sibling contracts relevant
  to the change and broadens uncertain selection without presenting an
  exhaustive standing audit as mandatory for every diff.
  [test](finite_review_context_includes_relevant_sibling_contracts)

- Review walks the pinned `{{ style_rules }}` document rule by rule, discovering
  rule families from the document itself (no fixed prefix enumeration in the
  prompt — the partial adapts to whatever conventions the consuming project
  uses). Each violation cites the rule id (whatever shape the project uses) and
  the offending file/line range. The prompt pins `{{ style_rules }}` so the LLM
  has the rules in its context.
  [test](build_review_prompt_includes_style_rule_conformance_walkthrough)

### Coverage reductions

- Existing semantic review flags unjustified loss of obligations or subjects,
  accepts demonstrated replacement coverage and coverage-preserving dependency
  precision, and routes findings through the ordinary review path.
  [judge](../tests/judges/coverage-reductions.md)

## Requirements

### Functional

#### Invariants — what must never happen

[Acceptance](#verdict-gate).

The five failure classes the gate guarantees against. These are the gate's
reason for existing; everything below them is mechanism.

1. **A spec claim is false in the code.** If a spec says X must happen, the
   implementation must make X happen. If a spec bans Y, the implementation must
   not contain Y. Includes multi-component contracts: parts {a, b, c} of a
   lifecycle either all land in the implementation, or the unfinished parts have
   a bonded successor doing the remaining work.

2. **A passing verifier is dishonest.** A deterministic verifier (`[check]`,
   `[test]`, or `[system]`) that asserts a tautology, mocks the thing it claims
   to test, or passes for the wrong reason is itself a divergence — the spec
   claim it cites is in fact unchecked. The gate distinguishes honest from
   dishonest verifiers; _all tests pass_ is not synonymous with _the spec is
   enforced_.

3. **A template directs agents toward spec-contradicting behaviour.** Planning,
   decomposition, and review templates are themselves system artefacts. They
   must operate consistently with the specs they drive — a template whose
   instructions contradict its spec produces cascading damage as the agent
   follows the template literally instead of the contract.

4. **A divergence sits in the working tree undetected, regardless of whether any
   merge is in flight.** Finite-diff review includes the current contracts and
   sibling context relevant to the change; it is not an exhaustive standing
   audit. Cross-file gaps unrelated to that surface, orphaned contracts, and
   accumulated pre-existing violations also need tree-wide review. Conformance
   is a property of the _current_ code-spec pair, not a historical artefact of
   past approvals.

5. **A load-bearing invariant is silently contradicted.** Five invariant
   categories: architectural decisions, data-structure choices, explicit
   constraints, non-functional requirements, and out-of-scope items. A change
   that contradicts any such invariant — in code or in a sibling spec — must
   surface, never slip. _Not_ a hard reject — clashes require human judgement
   (see Lanes, below).

#### Dimensions

[Acceptance](#verdict-gate).

The gate evaluates code on three dimensions, all together. Failure on any one is
a flag.

- **Conformance** — for every claim in the spec, there is a true code path that
  makes it real.
- **Style** — the implementation follows the consumer's code-style rules
  (conventionally consolidated in a style-rules document such as
  `docs/style-rules.md`, organised by language- or domain-specific family).
- **Test quality** — the tests follow the consumer's test-quality rules
  (typically in the same document); verifiers actually verify what they claim.

The specific rule families, their prefixes, and the path of the style-rules
document are consumer-defined. The gate evaluates against whatever rules the
consumer specifies; it does not impose a particular taxonomy.

These three dimensions are not separable concerns; they are aspects of the same
binary question: _is the code good enough to ship?_ They live in one gate by
design — fragmenting them produced the failure pattern this spec exists to
prevent.

#### Lanes

[Acceptance](#verdict-gate).

The gate has two response paths. The choice is dictated by the kind of failure
detected, not by stage or scope.

- **Hard fail (rule violation).** Code breaks an entry in the consumer's
  style-rules document, or a deterministic verifier (`[check]`, `[test]`, or
  `[system]`) that asserts a specific behavioural claim returns failure. There
  is no legitimate "keep this on top" path. The gate fails the check; per-stage
  recovery (same-bead recovery during worker/per-bead integration, push refusal
  plus remediation at push, remediation bead at standing) drives the response,
  all converging on _fix the code_.

- **Clarify (invariant clash).** Code (or a proposed spec change) contradicts a
  load-bearing invariant in a spec — one of the five categories from Invariant 5
  above. The right path requires human judgement, framed by the _three-paths
  principle_:

  1. **Preserve the invariant** — rework the change so the invariant still
     holds.
  2. **Keep the change on top of the invariant** inelegantly, with the debt
     recorded in the spec or notes.
  3. **Change the invariant** — update the spec and plan follow-up work to
     realign code.

  The three-paths principle is _guidance, not a rigid template_. A given clash
  may need fewer or differently-framed options, each phrased in terms concrete
  to the clash.

  Gate raises `loom:clarify` per
  [Inbox — Options Format Contract](inbox.md#options-format-contract) and waits
  for `loom inbox` resolution.

#### Commands

[Acceptance](#commands-surface--explicit-scopes-and-status).

The gate is one umbrella command, `loom gate`, with subcommands selecting what
kind of inspection or act path runs:

| Command                       | Kind                    | Purpose                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ----------------------------- | ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **`loom gate`**               | Help                    | Prints `loom gate --help` — the subcommand list with one-line descriptions. No verifiers run, no cache read.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| **`loom gate status`**        | Status                  | Reports historical results and current coverage eligibility for an explicit scope, without running verifiers. [Evidence](evidence.md#status-cache-1) owns projection and the bounded render target.                                                                                                                                                                                                                                                                                                                                                                                                 |
| **`loom gate audit`**         | All, inspection         | Runs `verify` then `review` for an explicit `--diff` or `--tree` scope. Inspection composition only: no bd writes and no marker mint. The act path is `mint`.                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| **`loom gate verify`**        | Deterministic           | Runs deterministic gate lanes for an explicit scope. Diff scope includes both the project pre-commit lane and spec annotations; file/tree scope is spec-annotation only.                                                                                                                                                                                                                                                                                                                                                                                                                            |
| **`loom gate check`**         | Deterministic, one tier | Runs only `[check]`-tier spec annotations for an explicit scope or exact `--target`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| **`loom gate test`**          | Deterministic, one tier | Runs only `[test]`-tier spec annotations for an explicit scope or exact `--target`, batched into one runner subprocess per invocation.                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| **`loom gate system`**        | Deterministic, one tier | Runs only `[system]`-tier spec annotations for an explicit scope or exact `--target`. The same system dispatcher serves stage-required finite verification and full-tree verification; this subcommand remains available for explicit tier inspection.                                                                                                                                                                                                                                                                                                                                              |
| **`loom gate review`**        | LLM judge, inspection   | Runs the LLM rubric for an explicit `--diff` or `--tree` scope. Inspection-only: emits `LOOM_FINDING:` records + terminal marker to stdout, makes no bd writes, and does not mint a marker. Push-eligible review consumes a typed `VerifiedScope` for the same resolved content/scope rather than a scalar exit-code flag.                                                                                                                                                                                                                                                                          |
| **`loom gate judge`**         | LLM judge, one lane     | Runs criterion-attached `[judge]` verifiers for an explicit scope or exact `--target`; skips the rubric walk. Inspection-only, like `review`.                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| **`loom gate rubric`**        | LLM judge, one lane     | Runs only the rubric walk for an explicit `--diff` or `--tree` scope; skips criterion-attached judges. Inspection-only, like `review`.                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| **`loom gate mint`**          | Act                     | Materializes findings into bd work. `loom gate mint -m/--molecule <id>` promotes that molecule's deferred remediation batches after original work drains; `loom gate mint --tree` runs the standing safety-net sweep and creates or updates ready remediation batches under one active work epic when actionable findings remain. `mint` has no per-bead, diff, file, spec-filter, or target surface. Clarify-route findings still materialize as one `loom:clarify` bead per finding so each carries one `## Options — …` block. See [_Findings and Minting_](findings.md#findings-and-minting-1). |
| **`loom gate verify-marker`** | Trust check             | Reads `.loom/marker.json`, validates the current workspace fingerprint, and exits 0 iff the marker is well-formed and current. Diagnostic use on the CLI remains valid. The `pre-push-checks` wrapper performs the hook-coverage validation defined in _Marker_ before short-circuiting any wrapped command; `verify-marker` is not registered as a standalone prek hook.                                                                                                                                                                                                                           |

Inspection runs one reviewer session, not a recovery or publishing loop. Human
live rendering goes to stderr; stdout carries result records and the terminal
marker, so rendered prompt examples cannot become findings in a parent's handoff
parser. `--bead` and internal context labels carry metadata, not scope filters
or mutation authority. Only a full diff review consuming matching
`VerifiedScope` can emit push-eligible completed review evidence; partial or
ordinary unverified inspection cannot manufacture that evidence.

Spec-specific target discovery is outside the gate command tree:
`loom spec <label> --targets` prints annotation targets for a spec, optionally
narrowed with `--tier <tier>`; `--plain` prints exact target strings one per
line for piping into `loom gate <tier> --target`.

##### Scope flags

[Acceptance](#scope-handling).

Gate subcommands do not infer a default verification scope. An inspection
subcommand invoked without an explicit scope or exact `--target` prints that
subcommand's help and runs no verifier, review, cache lookup, bd write, or
marker check. This includes bare `loom gate verify`: users must choose what
trust surface they are asking about.

The scope flag defines the **input set** — the files the gate is being asked
about. The admitted dependency contract determines affectedness, including
membership and absence observations, subject changes, and explicit global
responsibility. Stage applicability then determines eligible obligations.
Invalid or missing contracts fail admission; valid cold providers collect
observations through execution rather than being mistaken for unaffected work.

| Flag                           | Valid subcommands                                                                   | Input set / meaning                                                                                                                                                     | Typical caller                                                                                          |
| ------------------------------ | ----------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `--files <paths...>`           | `verify`, `check`, `test`, `system`, `judge`, `status`                              | Explicit file list. `verify --files` runs affected spec annotations eligible for the current stage, including required `[system]` checks; it runs no project hook lane. | pre-commit hook (`loom gate verify --files <staged files>`); local debugging                            |
| `--diff <range>`               | `verify`, tier commands, `review`, `rubric`, `judge`, `audit`, `status`             | `git diff <range> --name-only`, resolved to concrete base/head commits when the run is trust-bearing.                                                                   | worker self-check, driver per-bead post-integration verify, push-range verify/review, CI scoped to a PR |
| `--tree`                       | `verify`, tier commands, `review`, `rubric`, `judge`, `audit`, `status`, and `mint` | Every file in the workspace. `verify --tree` runs `[check]`, `[test]`, and `[system]` spec annotations.                                                                 | standing safety net, nightly CI, on-demand full inspection                                              |
| `--target <annotation target>` | `verify`, `check`, `test`, `system`, `judge`                                        | Fresh exact-target diagnostic; no verification-result reuse or project hook lane.                                                                                       | operator repeats one known target discovered via `loom spec <label> --targets`                          |
| `-m`, `--molecule <id>`        | `mint` only                                                                         | The molecule's deferred finding set.                                                                                                                                    | stabilization promotion                                                                                 |

`--target` is mutually exclusive with `--files`, `--diff`, and `--tree`.
Matching is exact against the annotation target string after markdown parsing;
no substring search, shell splitting, globbing, or positional selector fallback
exists. Zero matches fail loudly. A target that matches annotations in multiple
tiers fails on `verify --target` and suggests the tier-specific subcommand;
multiple criteria sharing the same target inside one tier are accepted and run
as one target selection.

An explicit `--target` request requires fresh verifier execution, not replay of
an earlier result. This intent bypasses verification-result reuse at both the
gate and provider boundary; it does not disable build or image reuse. If fresh
execution cannot be completed, ordinary failure/skip reporting applies rather
than substituting an earlier pass. Exact-target diagnostics cannot authorize
publication. Normal verification may instead consume
[admissible equivalent-execution evidence](evidence.md#cache-and-evidence-reuse).

`--bead` is not a deterministic scope. Deterministic trust paths use explicit
diffs because the git range is the content being verified. `loom gate review`
may accept `--bead <id>` only as intent/context metadata paired with
`--diff <range>`; `review --bead <id>` without `--diff` is invalid.

`--spec` is not a gate filter. Work-surface affectedness decides which spec
annotations run, and spec-specific target discovery uses
`loom spec <label> --targets` instead. Automated trust paths therefore cannot
narrow away sibling-spec obligations by passing a spec label.

A `--diff <range>` that git itself rejects — an unknown commit, or `@{u}` when
the branch has no upstream — is a **hard error**: the gate exits non-zero naming
the range, rather than degrading to an empty input set. A trust-bearing diff run
resolves the range to concrete base/head commits and records those commits in
its `GateRun`. A valid range that matches no files is a legitimate empty scope;
a shorthand like `--diff HEAD` remains a diagnostic working-tree-vs-HEAD mode
but is never marker-eligible.

##### Deterministic verify lanes

[Acceptance](#commands-surface--explicit-scopes-and-status).

`loom gate verify` has one deterministic contract across all callers:

| Invocation                               | Project hook lane                                                    | Spec annotation lane                                                                                     |
| ---------------------------------------- | -------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `loom gate verify --files <paths...>`    | none                                                                 | Affected `[check]`, `[test]`, and `[system]`, subject to explicit stage declarations                     |
| `loom gate verify --diff <base>..<head>` | `prek run --hook-stage pre-commit --from-ref <base> --to-ref <head>` | Affected `[check]`, `[test]`, and `[system]`, subject to explicit stage declarations                     |
| `loom gate verify --tree`                | none                                                                 | Full-tree `[check]`, `[test]`, and `[system]`                                                            |
| `loom gate verify --target <target>`     | none                                                                 | Fresh execution of the exact matched target in its tier; no scope filtering or verification-result reuse |

The project hook lane is first-class trust, not hidden magic: project policy
lives in `.pre-commit-config.yaml`, and prek decides hook file selection,
`always_run`, and filename passing. Loom does not reinterpret project hook
policy. The project hook lane runs before spec annotations; if a hook modifies
the working tree, `verify` exits non-zero with a tree-modified-by-hook failure
so the caller stages/commits or rolls back before retrying. Successful `verify`
requires the tree to remain unchanged after hooks.

Diff-scope recursion is guarded by the parent gate run, not by a
project-specific hook id. When a parent `verify --diff` invokes prek and the
project's pre-commit stage includes `loom gate verify --files`, the nested files
invocation records a skipped gate event with reason `parent-diff-gate` and exits
0 because the parent run will execute the annotation lane after prek returns.
The canonical hook id may be used as an optimization, but correctness cannot
depend on it.

Affected deterministic annotations are eligible under finite `verify --diff` and
`verify --files` by default. A spec must explicitly declare later-stage
applicability to postpone a check. The annotation tier describes how the claim
is verified, not when to omit it; an absent stage declaration does not mean
publication-only. Selected systems use the existing verify planning and system
dispatch path, with no conformance-only hook or parallel verification procedure.
Strategy declarations follow
[spec-owned verification strategy](../docs/spec-conventions.md#verification-strategy).

Stage applicability belongs to the criterion's obligation, not to a target or
runner globally. An explicitly publication-only obligation does not itself
require earlier-stage execution when its inputs change. Another eligible
criterion can still require the shared verifier at that earlier stage; deferral
is not a prohibition on executing the unit for other obligations. Execution
sharing and evidence admission follow their ordinary rules. Full-tree and
mandatory publication coverage are not reduced. Worker capability acceptance
remains governed by
[Sandbox-capability results](verify.md#sandbox-capability-results): missing
capability is not an observed pass or publication evidence. In particular,
[absent-tool hook no-ops](pre-commit.md#stage-composition) cannot satisfy host
publication coverage. Affectedness uses the verifier-input contract; expense
alone does not make a selected check optional.

The composition: `loom gate audit` ≡ `loom gate verify && loom gate review` for
the same explicit `--diff` or `--tree` scope. Both are inspection paths; `audit`
produces no bd writes. The act path is `loom gate mint`, which walks and writes;
see [_Findings and Minting_](findings.md#findings-and-minting-1).

##### Automatic applicability context

[Acceptance](#commands-surface--explicit-scopes-and-status).

Existing entrypoints establish the verification applicability context; users do
not select it with a public stage flag:

- Standalone finite `loom gate verify --files` and `--diff` use feedback
  applicability, as do ordinary pre-commit, worker self-check, and per-bead
  integration requests. Criterion-local publication deferrals may wait.
- The driver's publication gate and the pre-push chain establish publication
  applicability for their actual range. Operator-manual pushes without a marker
  get the same deterministic publication obligations, not feedback defaults.
- Explicitly rehearsing the existing pre-push chain with
  `prek run --hook-stage pre-push --from-ref <base> --to-ref <head>` requests
  publication coverage without adding a second verification entrypoint.
- `verify --tree` retains full deterministic annotation coverage regardless of
  deferrals. Exact-target diagnostics retain their fresh, non-authorizing
  semantics.

Caller context is parsed and resolved at the existing driver/hook boundary and
carried through the shared planner. A required publication handoff with missing
or invalid context cannot silently become a weaker feedback request or satisfy
publication with feedback-only evidence. Marker absence is normal for manual
pushes: the hook path establishes its context without requiring a prior marker.
Nested hooks retain the parent-run recursion guard; no stage override bypasses
it or changes hook ordering.

Applicability determines required checks, not permission to publish. Rehearsal,
full-tree inspection, or requesting publication coverage does not mint a marker,
create review coverage, or turn worker capability skips into host evidence.
Driver authority still requires the existing receipt and
[publication-attempt admission](#publication-attempt-handoff); eligible unit
results remain subject to ordinary evidence rules. Manual pushes retain the
[existing deterministic hook policy](pre-commit.md#source-of-truth-files), not
an added requirement for driver-only review. Concrete internal context carriers
remain implementation design, not a new public stage-selection interface.

#### Stages

[Acceptance](#commands-surface--explicit-scopes-and-status).

The same gate serves planning, worker feedback, integration, stabilization,
publication, and standing audits. Scope and cost of failure differ; each stage's
trust boundary is explicit.

| Stage                    | Where                                                                                                                                                                                                                         | Scope                                                                | Cost-of-failure                                                                                                                                                                                                                                             | Primary catches                                                                                                                                                                                                                        |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Plan**                 | `loom plan [SPEC_LABEL ...]`                                                                                                                                                                                                  | Anchored specs plus siblings touched during interview                | Lowest — no code yet                                                                                                                                                                                                                                        | Missing claims, weak claims, missing verifier surfaces, invariant clashes in proposed spec changes                                                                                                                                     |
| **Worker self-check**    | In the bead container before `LOOM_COMPLETE`: `loom gate verify --diff <bead-base>..HEAD` plus prompt-level self-review                                                                                                       | The bead branch's committed work against its injected base range     | Low — the agent is still in-session                                                                                                                                                                                                                         | Formatting/hook failures, affected stage-required deterministic failures, obvious criteria/style misses the agent can fix before final marker                                                                                          |
| **Per-bead integration** | In `.loom/integration` after rebase/ff: `loom gate verify --diff <pre-integration-head>..HEAD`                                                                                                                                | The exact commits just integrated into the loom workspace            | Medium — one bead's worth                                                                                                                                                                                                                                   | Cross-bead deterministic breakage after integration, project pre-commit failures, affected stage-required deterministic failures. On failure, integration rolls back and the same bead retries with the gate log in `previous_failure` |
| **Stabilization**        | `loom gate mint -m <molecule-id>` after no original non-deferred work remains ready                                                                                                                                           | The molecule's `loom:deferred` remediation beads                     | Medium — amortized over a molecule, not one tiny finding                                                                                                                                                                                                    | Promotes deferred remediation batches so the loop drains them before push; coalesces repeated finding hashes instead of reminting tiny beads                                                                                           |
| **Push**                 | Fetch/rebase to `origin/<integration-branch>`, resolve the remote tip and `HEAD` to concrete OIDs, run the actual prek pre-push chain for `<remote-oid>..<head-oid>`, then `loom gate review --diff <remote-oid>..<head-oid>` | The actual push range, not merely the molecule's original base range | Highest — **blocks push**. `GateSuccess` is constructible only from matching `VerifiedScope`, `ReviewedScope`, pre-push hook coverage, current evidence/attempt admission, and clean tree/config/range fingerprints; the marker is minted from that receipt | Conformance gaps in the pushed range, project pre-push failures, integrity-gate findings in affected annotations, review concerns, dispatch errors, origin-advanced races                                                              |
| **Standing safety net**  | `loom gate audit --tree` for inspection; `loom gate mint --tree` to act (on-demand, nightly CI, scheduled)                                                                                                                    | Entire spec tree × entire implementation                             | Catches **verifier-input under-reporting** — any verifier a finite scope would have skipped because its derived input set was too narrow is surfaced here                                                                                                   | Cross-file incoherence finite diffs did not surface, contracts orphaned across PRs, accumulated style/test regressions, template-vs-spec drift (Invariant 3), surface drift, verifier-reported input sets that are too narrow          |

The plan stage has no separate command invocation — the agent runs the rubric
inline during the planning interview, and `loom plan` is the surface that opens
that interview. Worker self-check is prompt-level feedback; it is not
marker-eligible and cannot authorize a push. The driver's per-bead integration
stage is deterministic only. Focused per-bead LLM review is not part of the
default hot path; the worker's self-review happens inside the implementation
session, and the authoritative LLM review runs at molecule completion over the
actual push range.

The push stage is **non-optional and load-bearing across every execution mode of
`loom loop`** — default active work epic, explicit bead/epic roots, and parallel
dispatch. It synchronizes with origin before verification, rebases local
integration commits when origin advanced, verifies the exact range that would be
pushed, reviews that same range, mints a marker only for that range, and pushes
inside the same critical section. A non-fast- forward or origin-advanced race
invalidates the prior gate result; the driver fetches/rebases and reruns the
gate instead of reusing stale evidence.

The verdict is encoded in [`GateOutcome`](loop.md#loop-outcome-types): `Success`
only when the typed gate evidence matches the final pushable state; `Fail` on
any failure with the reason explicit; `NoGate` only for legitimate "no work to
gate" terminals (`NoBeadsReady`, `SelectionPartial`). The `GateSuccess` struct
is sealed, so a clean `loom loop` exit without the gate actually firing is
unrepresentable. The standing safety net is scheduled, not load-bearing for any
individual push — its job is to catch verifier- input under-reporting over time,
not to replace the push gate.

### Worker and per-bead integration checks

[Acceptance](#success-criteria).

**Review findings.** Authoritative LLM review runs at molecule completion over
the actual push range. Review findings carry an explicit `route` field in
addition to the concern token (see
[Findings — Emit shape](findings.md#emit-shape)):

| Route      | Meaning                                                                                                                                                                                                                    | Driver action                                                                                                                       |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `blocking` | The pushed range is not acceptable: acceptance criteria unsatisfied, touched code violates style/test-quality rules, verifier added or changed by the molecule is dishonest/too narrow, or deterministic verify regressed. | Refuse the push and create or reuse same-molecule remediation work; no marker is minted.                                            |
| `deferred` | The finding is real but broader than the pushed range's immediate acceptance surface: adjacent or pre-existing drift, cross-spec / standing-safety-net issue, or cleanup outside the touched surface.                      | Merge the finding into the molecule's `loom:deferred` remediation bead for the lead spec; do not make it ready until stabilization. |
| `clarify`  | Human intent is required, typically an invariant clash with multiple valid resolution paths.                                                                                                                               | Create or update one `loom:clarify` bead for the finding hash, preserving the required `## Options — …` block.                      |

The **Concern token** still names the issue class (`spec-coherence-fail`,
`style-rule-violation`, `verifier-bypass`, `invariant-clash`, …) and determines
the target variant. The route determines workflow behaviour. Token alone is too
coarse: for example, `spec-coherence-fail` is blocking when it invalidates the
pushed work's own criterion, but deferred when it identifies adjacent drift
outside the pushed surface.

[Findings' explicit stream/terminal pairing](findings.md#emit-shape) governs
review completion, with [Protocol](protocol.md#phase-admission) owning shared
message framing and phase admission. The concern summary is not per-finding
routing authority; each resolved finding's `route` controls the workflow effect.
A clarify-route finding whose evidence lacks a well-formed options block falls
back to `loom:blocked` with cause `clarify-without-options` rather than a
stranded clarify. Push is held until clarify beads in the molecule are resolved
via `loom inbox`.

##### Standing-safety-net checks

[Acceptance](#verdict-gate).

`loom gate verify --tree`, `loom gate review --tree`, and
`loom gate mint --tree` form the standing-safety-net triad. The first two are
inspection-only and run independently (or compose via `loom gate audit --tree`);
the third is the act surface that produces remediation beads. Mechanical-only
inspection is fast and frequent; the full sweep + mint is rarer.

`loom gate verify --tree` exercises every audit at tree scope: every `[check]` /
`[test]` / `[system]` verifier, all linters, all `[check]`-tier walks the
consumer has registered, walking every spec and every implementation file.

`loom gate review --tree` runs the LLM rubric against the whole spec set ×
implementation. The finite-diff rubric checks apply, scoped to the tree rather
than a diff. Additional safety-net-only checks:

- **Template-vs-spec drift** (Invariant 3 enforcement). Reads every template
  loom uses (embedded in the loom binary, plus any consumer-provided overrides)
  against every spec in the consumer's spec tree. Flags any template instruction
  that contradicts a spec claim. Hard fail conceptually, but surfaced as a `bd`
  issue (no "merge to refuse" at the standing safety net). Concern token:
  `template-spec-drift`; the rubric body lives in the review prompt's
  _Template-vs-Spec Drift Walk_ partial, gated on `--tree` scope.

- **Cross-spec clash.** Two sibling specs in the workspace make incompatible
  claims about a shared surface — e.g. one spec defines a contract one way, a
  sibling references it differently; one spec's command-set table conflicts with
  another's prose. Concern token: `cross-spec-clash`. Target is
  `Criterion { spec, anchor }` naming the side the reviewer considers primary;
  `bonds` lists every spec the clash spans, and the other side(s) appear
  verbatim in `evidence` prose. This exhaustive standing walk is gated on
  `--tree`, not access to sibling context. Finite review receives relevant
  current sibling contracts through
  [Templates](templates.md#acceptance-context-and-progressive-disclosure) and
  applies its existing conformance/invariant-clash rules to the changed surface.
  Uncertain relevance broadens context; it does not silently hide a constraint
  or require an unrelated whole-tree audit on every diff.

- **Spec-conventions violation.** A spec violates a rule from
  `docs/spec-conventions.md` that the structural integrity gate cannot detect
  deterministically — un-promoted tentative annotations (annotations carrying
  placeholder language the spec author intended to revise before commit), a
  testable claim authored without any verifier annotation, prose that should be
  a success-criteria bullet but lives as flat prose, or any other convention
  violation that requires reading the spec's intent rather than walking a
  structural check. Concern token: `spec-conventions-violation`. Target is
  `Criterion { spec, anchor }` naming the offending location. (Mechanical
  violations — multi-annotation criteria, unresolved annotations, stub-pointing
  — are caught by the integrity gate at every scope, not here.) Gated on
  `--tree` scope.

`loom gate mint --tree` walks both the deterministic verifiers (`verify` side)
and the LLM rubric (`review` side), then mints typed findings from both into
remediation beads. The walk semantics are identical to `verify --tree` +
`review --tree` running together; the act is what `mint` adds. Standing-stage
findings route through the spec/work-epic lifecycle in
[Todo](todo.md#spec-and-work-epic-lifecycle):

- **Spec epics are metadata only** — every bonded indexed spec must have exactly
  one `loom:spec spec:<label>` epic. A missing spec epic is created, then
  immediately closed, as metadata; duplicate spec epics are a structural
  invariant violation and `mint` refuses before creating remediation work. Work
  beads are never parented to spec epics.
- **Actionable tree findings share one work epic** — after suppression, dedup,
  and structural validation, all remaining tree-scope fix-up, blocked-clarify,
  and clarify beads are parented under one standing remediation work epic for
  the mint run, regardless of lead spec. The driver labels that epic as the sole
  `loom:active` work epic so bare `loom loop` runs it.
- **No actionable findings means no new epic** — if the tree sweep finds no
  unsuppressed actionable findings, or every finding dedups to existing live
  work, `mint --tree` creates no work epic and leaves the current `loom:active`
  bookmark unchanged.

This is the safety property — findings about a spec with no active work get one
loopable work container, not silently dropped or scattered across spec-local
epics, while spec-local cursor metadata remains on spec epics.

See [_Findings and Minting_](findings.md#findings-and-minting-1) for the full
deferred remediation processing flow, dedup mechanism, and emit shape.

This behaviour is uniform for `mint --tree` across the workspace: `mint` has no
`--spec` filter; lead-spec selection comes from each finding's typed `bonds` and
target for grouping/labels, not from CLI narrowing or parent-epic selection.

Invariant clashes surfaced at the standing safety net raise `loom:clarify` under
the same standing remediation work epic.

##### Surface-conformance audit

[Acceptance](#success-criteria).

A deterministic audit (no LLM call) that diffs the consumer's spec-declared
user-facing surface against the compiled binary. Closes the class of failure
where the spec mandates a command or flag the binary never grew (or fails to
remove one the spec marked removed). Implemented as a `[check]`-tier verifier
rather than a separate subcommand: the consumer annotates the relevant spec
criterion with
`[check](<command that diffs declared surface against the binary>)`. See
[Harness](harness.md#surface-conformance) for the four hard-fail dimensions and
audit triggers.

**Boundary with `loom gate review`'s style-rule walk.** Help-text wording is
**not** a surface-audit dimension. CLI-style requirements (e.g. a short
single-sentence help line, no implementation references) live under the
LLM-judged style-rule walk so spec prose can be polished without churning a
deterministic gate. The surface audit checks that commands and flags exist with
the right names and grouping — nothing about how they describe themselves.

#### Gate evidence and marker

[Acceptance](#gate-evidence-and-marker-coverage).

Gate trust is represented as typed evidence in the normal JSONL event stream,
not as ad-hoc sidecar receipts. Every `loom gate` invocation that runs work
writes a gate log under `.loom/logs/gate/` and emits `driver_event` records with
typed `DriverKind` values. The log is the full replay/audit source while
retained;
[compact retained safety evidence](evidence.md#retained-safety-evidence)
preserves the required admitted facts and replay witnesses beyond transcript
expiry. SQLite status and lookup indexes are rebuildable views over those
evidence sources, not authority and not the sole surviving record of safety
history.

##### GateRun lifecycle

[Acceptance](#gate-evidence-and-marker-coverage).

`GateRun` is the typed record of one gate invocation, whether it succeeds,
fails, skips, or aborts. A run emits lifecycle events:

1. `gate_run_start` immediately when the invocation is accepted.
2. `gate_run_scope` after the scope is resolved to concrete files, commits,
   target matches, and config digests.
3. `gate_run_lane` for each project hook lane, spec annotation tier, review
   lane, or skipped nested invocation.
4. `gate_run_end` carrying the serialized `GateRun` summary.

A `gate_run_start` without a matching `gate_run_end` is meaningful: the run was
interrupted or the process died before it could finish. When possible, an
aborting process emits an explicit aborted end event, but consumers cannot
require it. `GateRun` contains compact pass identity (tier/target/runner or hook
id, duration, exit), full failure and skip details, scope digests, relevant
config digests, and log path; it does not store large successful stdout bodies.

##### Gate success receipt

[Acceptance](#execution-and-admissible-evidence).

`VerifiedScope` is a sealed deterministic-success value derived from a passing
`GateRun`; `ReviewedScope` is the corresponding sealed value derived from a
passing review run. Push-eligible review consumes typed `VerifiedScope` evidence
for the same resolved content and scope. Manual diagnostic review may run
without that evidence, but cannot feed the success receipt or marker minting.
The retired `--verify-exit` scalar is not a trust input.

`GateSuccess` is the sealed, push-authorizing receipt carried by
`GateOutcome::Success`. Its non-optional evidence consists of the matching
`VerifiedScope` and `ReviewedScope`, pre-push hook coverage, the resolved push
range and tree fingerprint, and paths to the gate logs that contain successful
matching `GateRun` end events. `GateSuccess::new` is the sole construction path.
It returns `GateFail` unless the scopes describe the same content, range, tree,
and relevant configuration; every covered hook has passed for that range; the
coverage fingerprints match both scopes; the gate logs exist and end
successfully; and the molecule is clean under
[Loop's current completion admission](loop.md#completion-admission), not Beads
progress or workspace-disposal permission alone. The structural seal prevents
callers from bypassing these checks with a struct literal.

Receipt construction and consumption also require current
[evidence admission](evidence.md#cache-and-evidence-reuse) and
[publication-attempt context](#publication-attempt-handoff). Matching successful
logs are necessary, not sufficient: disqualified outcomes, unmet freshness, or
an ended attempt cannot acquire authority through a sealed receipt. A receipt
does not make the facts checked at its construction permanently current.

Matching is content-based, not argv-string-based: same workspace tree, same
scope kind, same resolved base/head commits for diff scope, same changed-file
digest, no gate-narrowing filters, required lanes present, and matching relevant
config digests (`.pre-commit-config.yaml`, `loom.toml`, and the current spec
annotation and verification-strategy digests).

##### Marker

[Acceptance](#gate-evidence-and-marker-coverage).

`MarkerProof` is the content-addressed trust-bearing artifact the driver-side
push gate mints on `GateSuccess` and prek's pre-push hook wrapper consumes to
avoid rerunning work already proven for the exact push. Its purpose is to make
"the gate ran cleanly at this exact tree, range, config, and hook coverage" a
typed Rust value rather than an ad-hoc filesystem stamp.

The mint authority lives in `loom-gate::marker`. The constructor
`MarkerProof::from_gate_success` is `pub(crate)`, accepts the sealed
[`GateSuccess`](#gate-success-receipt), computes the current workspace
fingerprint, and returns a `MarkerProof`. No code path outside the marker module
can mint a marker; no code path outside the gate-invocation module can construct
the `GateSuccess` that mint requires. The bead-container agent cannot mint,
regardless of what it writes to disk or emits on stdout.

Architecture-bearing marker fields include:

- schema version;
- HEAD commit SHA (informational) and HEAD tree OID;
- porcelain-clean assertion at mint time;
- resolved push range (`from_ref`, `to_ref`);
- `.pre-commit-config.yaml` digest;
- references to the gate log / `GateRun` ids that produced the `VerifiedScope`
  and `ReviewedScope`;
- the set of pre-push hook ids / entries that passed in the verified pre-push
  stage.

The `commit_sha`, `tree_oid`, and range OIDs carry validated OID newtypes.
Malformed OIDs are rejected at deserialize time as typed parse errors rather
than reaching fingerprint comparison.

##### Marker validation and hook coverage

[Acceptance](#execution-and-admissible-evidence).

Marker validation is two-layered:

1. **Workspace fingerprint.** The current worktree must be porcelain-clean,
   HEAD's tree OID must match the marker, the marker schema version must be
   supported, and the `.pre-commit-config.yaml` digest must match.
2. **Hook coverage.** The pre-push hook currently wrapped by `pre-push-checks`
   may short-circuit only when the marker's typed `GateSuccess` proves the same
   resolved push range, a successful pre-push `GateRun` includes that hook
   id/entry as passed, and successful `VerifiedScope` and `ReviewedScope` exist
   for the same push range.

Referenced verification evidence must remain admissible under
[failure invalidation](evidence.md#failure-invalidation),
[confirmed counterexample admission](evidence.md#confirmed-counterexamples-across-campaigns),
[claim-specific freshness](evidence.md#claim-specific-freshness), and the
[publication-attempt boundary](#publication-attempt-handoff). Matching
fingerprints and earlier successful logs do not override evidence
disqualification or authorize reuse-disabled evidence on a later attempt.

If any element is absent or mismatched — dirty tree, different tree OID, changed
pre-commit config, different push range, missing/deleted gate log evidence, hook
not covered, failed lane, or missing review success — the wrapper falls through
and executes the underlying hook. Marker validation is an optimization, never a
trust bypass.

##### File location and lifecycle

[Acceptance](#gate-evidence-and-marker-coverage).

Marker lives at `.loom/marker.json` in the loom workspace — a single file,
overwritten on each mint. Atomic write uses `<path>.tmp` + rename. The file
lives in the loom workspace only; operator and bead workspaces do not contain a
trusted marker. Gate logs referenced by the active marker are evidence;
retention should preserve them while the marker is active when possible. If
positive run evidence is missing, validation fails and hooks run. Separately,
[retained safety evidence](evidence.md#retained-safety-evidence) remains
required for current admission; neither falling through to a hook nor an old
marker can bypass lost or disqualifying safety history.

##### Mint trigger

[Acceptance](#execution-and-admissible-evidence).

The driver-side molecule-completion push gate at the loom workspace (per
[Loop — Verdict Gate](loop.md#verdict-gate)) is the sole mint trigger. The
sequence is:

1. Push gate acquires `index.lock` and fetches origin.
2. If `origin/<integration-branch>` advanced, the driver rebases local
   integration commits onto it; conflicts route through recovery and no marker
   is minted.
3. The driver resolves `origin/<integration-branch>` and `HEAD` to the concrete
   OID endpoints of the actual push range.
4. The deterministic push gate runs the actual prek pre-push chain for that
   range. The chain's `loom gate verify --diff <range>` hook emits its own
   `GateRun` / `VerifiedScope` evidence.
5. On deterministic success, the driver runs `loom gate review --diff <range>`
   and constructs `ReviewedScope` on clean review.
6. `GateSuccess` is constructed from the matching evidence; the marker is minted
   and written.
7. `git push origin <integration-branch>` runs inside the same critical section.
   Failure or interruption ends the attempt under
   [Publication-attempt handoff](#publication-attempt-handoff); any retry
   requires new attempt admission, not replay of its marker. A non-fast-forward
   additionally forces fetch/rebase against the actual remote state.
   Independently admissible unit evidence remains reusable under the ordinary
   rules.

Per-bead integration steps acquire the same lock for rebase + ff + verify but
release without minting. The push gate waits for any in-flight integration to
release before starting its own critical section.

##### Consumer contract

[Acceptance](#execution-and-admissible-evidence).

`loom gate verify-marker` is a diagnostic marker-validation subcommand. It reads
`.loom/marker.json`, checks the current workspace fingerprint, and exits 0 on a
current marker, non-zero on a missing, stale, malformed, or unsupported marker.
The diagnostic on stderr names the specific `MarkerError` variant for human
debugging but is not the machine-readable contract.

The pre-push chain consumes markers through the repo-local `bin/pre-push-checks`
wrapper, while the Git hook shim that invokes prek comes from `wrix.prekHooks` —
see [Pre-Commit — Marker integration](pre-commit.md#marker-integration). The
wrapper validates both fingerprint and hook coverage for the hook it wraps,
short-circuits on covered success, and execs the underlying command on marker
absence or mismatch. `loom gate verify-marker` is not registered as a standalone
prek hook; missing marker is the normal operator-manual condition and must fall
through, not abort.

##### Forgery resistance and workspace boundary

[Acceptance](#gate-evidence-and-marker-coverage).

The marker is forgery-resistant against tree-state forgery, stale markers after
edit, hook-coverage forgery, and agent verifier-execution forgery. A
hand-written JSON file can match the JSON shape but cannot construct the sealed
`GateSuccess` evidence it references, cannot make a different tree/config/range
pass validation, and cannot cause an uncovered hook to short-circuit.

The marker is workspace-local — never trusted across machines or across clones
of the same repo. The driver's loom workspace, the operator's `/workspace`, and
bead workspaces are separate clones. The only writer is the driver-side push
gate in the loom workspace; operator and bead workspaces fall through to the
full pre-push hook chain. CI never reads the marker; CI re-derives checks in its
own sandbox.

### Non-Functional

1. Gate evidence stays bound to the current tree, configuration, and resolved
   scope; stale evidence cannot authorize a push.
2. Cached status reporting follows Evidence's
   [bounded render target](evidence.md#status-cache-1), not an arbitrary-corpus
   latency guarantee or permission to omit current coverage limitations.
3. Verifier dispatch remains repository-agnostic with explicit input-contract
   admission. Invalid discovery fails loudly; valid cold execution and justified
   broad responsibility remain distinct from configuration failure.

## Out of Scope

- Additional general-purpose trusted provider backends for projects without Nix.
  The pilot targets the actual Loom/Wrix Rust/Nix integrations; shared APIs
  remain repository-agnostic. This does not remove tracked Rust analysis,
  existing native feedback, or explicit effectful lanes, and unsupported
  providers do not gain a heuristic admission fallback.
- A new general-purpose incremental-query engine, speculative scheduler, or
  separate persistent trace/result-cache service. Tracked-input invalidation and
  existing gate evidence/Nix reuse bound the design.
- Coverage-based selectors trusted to omit required Rust tests, or a promise of
  precise function-level Rust impact analysis. Property grouping belongs to
  [Verify](verify.md#selective-property-campaigns); shared suite policy belongs
  to [Tests](tests.md).
- Inferring semantic test adequacy from input closure, hermeticity, or Rust
  types. Those boundaries do not prove that a checker enforces the intended
  rule.
- Replacing mandatory local publication coverage with nightly or CI-only checks;
  hook policy is owned by Pre-commit.

The gate enforces; it does not own:

- The _content_ of the consumer's style-rules document — which rules exist, how
  they're organised, what prefixes the consumer uses. The gate references the
  rules; the rules are authored by each consumer.
- The _content_ of `[check]` / `[test]` / `[system]` / `[judge]` verifiers. The
  gate runs them; they live in the consumer's repo.
- The _organisation_ of the consumer's verifiers — whether the `[check]`-tier
  walks live in a dedicated crate, are scattered across source crates, or are
  shell scripts is the consumer's choice. The gate dispatches whatever
  annotation says, however the consumer chooses to back it.
- Performing workflow effects (push, merge, bead lifecycle, remediation bonding,
  molecule progress). Loop owns orchestration; Gate owns the admissibility of
  evidence and authorization consumed at those boundaries.
- The `loom:clarify` resolution channel itself — `loom inbox` is the surface,
  defined in [Inbox — Inbox Modes](inbox.md#inbox-modes).

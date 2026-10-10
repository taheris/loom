# Workflow simulation

Defines production-connected model conformance, bounded campaigns,
reproducibility, adoption, and cost evidence.

## Problem Statement

Selection, caching, and publication interact across component boundaries;
checking each component alone can miss unsafe sequences. This spec tests the
composed contracts against a bounded reference model and real production paths.

## Architecture

The model composes component contracts; production adapters expose their actual
transitions and outcomes for comparison. Related owners: [gate](gate.md),
[verify](verify.md), [evidence](evidence.md), [findings](findings.md),
[loop](loop.md), [pre-commit](pre-commit.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Ownership

[Acceptance](#success-criteria).

This spec owns model construction, simulation, symbolic checking, implementation
conformance, reproducibility, and model-verification cost. Behavioral contracts
remain with their components:

- [Gate](gate.md) owns scope/stage composition, review, and publication.
- [Verify](verify.md) owns obligations, provider/input admission, selection,
  property campaigns, and execution.
- [Evidence](evidence.md) owns reusable results and retained safety history;
  [Findings](findings.md) owns defect records and remediation.
- [Specs](specs.md) owns packages and criterion identity; [Loop](loop.md),
  [Todo](todo.md), [Plan](plan.md), and [Inbox](inbox.md) own their workflow
  phases.
- [Templates](templates.md) owns acceptance-context delivery and recovery.
- [Tests](tests.md) owns native suites and shared test fixtures.
- [Pre-Commit](pre-commit.md) owns hook composition and shortcut policy.
- [Spec conventions](../docs/spec-conventions.md) own package document roles,
  strategy declarations, and acceptance syntax.

Quint models intended behavior; Nix constrains execution; Rust restricts valid
plans and evidence. These mechanisms are complementary, not interchangeable
proofs. A passing model does not establish implementation conformance, complete
input tracking, verifier honesty, or correspondence with human intent.

Models live with their owning spec package. `model.qnt` is optional generally;
the whole-gate pilot lives at `specs/gate/model.qnt`, composing its sibling
contracts rather than requiring one model per extracted owner. Imports and
supporting inputs participate in discovery under Specs and affectedness under
Verify. Cross-label claim moves preserve the explicit safety-history linkage
owned by Evidence and are covered by conformance, not hidden by renamed claims.

### Whole-gate reference model

[Acceptance](#success-criteria).

The model covers discovery, integrity and pending handling, scope resolution,
input resolution, planning, selection, shared execution, reuse, dispatch,
aggregation, review, authorization, invalidation, and recovery. Project hooks
and all four annotation tiers participate, including their applicability at
planning, worker feedback, integration, publication, and standing verification.
The model follows the owning contracts, rather than defining alternative
selection, evidence, or push rules. It includes Gate's obligation-level stage
applicability when criteria with different declarations share an execution unit.
It also models
[automatic caller context](gate.md#automatic-applicability-context) through
actual driver/hook entrypoints: publication coverage cannot be reduced by a
missing marker or a lost required handoff, while a hook rehearsal does not
acquire publication authority merely by selecting stronger coverage.

Pending readiness is an explicit purpose within Verify's admitted planner, not
an unscoped integrity execution path or ordinary pending-criterion pass. The
model includes absent definitions, invalid offered providers, affected
stage/tier eligibility, shared targets with different purposes, and incomplete
required readiness. Host publication cannot treat a missing-tool wrapper's exit
zero as coverage; worker capability feedback retains its separate policy.

Task preparation includes Loop's distinct resolved criterion and remediation
goals. Valid finding-driven repair can address malformed acceptance without
fabricated criteria; bad ordinary references cannot switch purpose to bypass
resolution. Status/decomposition projections distinguish historical results from
current coverage and retained blockers without executing new checks or
manufacturing authority.

A simple reference procedure discovers and evaluates every obligation applicable
to the same snapshot, scope, and stage without result reuse or selection
shortcuts. Applicability is derived independently of the optimizer's selected
targets and cached traces. The optimized procedure is modeled against this
reference, including inventory and input construction, tracked discovery, cache
validation, batching, and concurrent completion; it does not start with an
assumed-correct input list.

For stable valid deterministic inputs, per-obligation outcomes agree. Effectful
execution and review use Evidence's admissibility/freshness rules, Gate's
authorization rules, and modeled external outcomes rather than assuming
identical LLM or host results on rerun. The model distinguishes reuse of earlier
results from the consumption of fresh evidence within one publication attempt.
It models Gate's attempt-ending boundary on failed/interrupted pushes, process
loss without a recorded outcome, and new-attempt admission on retry, including
remote changes despite an unknown push outcome. Hook entry and retries cannot
bypass Gate admission through a separate stamp or authorization shortcut.

Result import is a modeled transition through
[Evidence's Nix-cache admission](evidence.md#nix-cache-result-admission), not an
execution or fresh observation. Conformance includes both admissible imported
verification results and rejected artifacts, including imports after recorded
failures. Cache provenance and environment assumptions remain explicit. The
model includes changes to Evidence's Nix-derived signing trust and re-admission
of already-imported results; it does not treat a successful substitution as a
pass.

The reference and optimized procedures both consume admitted counterexample
knowledge under
[Evidence's claim-level rule](evidence.md#confirmed-counterexamples-across-campaigns).
The model includes confirmation, regression replay, resolution, and attempted
publication using a routine pass after a deeper campaign has found a violation.
Different campaign identities cannot hide the known defect; a timeout remains
incomplete exploration rather than a counterexample. Confirmation and resolution
are modeled transitions, not unchecked flags supplied by the adapter.

The model separates expiring transcripts, rebuildable indexes, and
[Evidence's retained safety evidence](evidence.md#retained-safety-evidence). It
includes recording/consolidation, expiry, interruption, and recovery rather than
assuming history lives forever in logs. Conformance exercises real retention and
restart: ordinary old transcripts disappear while admission facts and required
witnesses survive. Missing required safety evidence cannot be interpreted as no
failures or counterexamples; compact records cannot manufacture a pass or resume
an attempt.

The model includes
[shared local-workspace safety history](evidence.md#local-workspace-safety-history):
operator and integration invocations admit and consume facts through the same
history, with concurrent recording and validation/use interleavings. Conformance
exercises actual checkout association and worker isolation, not two independent
histories or an adapter-supplied trusted workspace identity. Shared facts do not
transfer clone-local markers or publication attempts; the model does not assume
a global cross-machine ledger.

External tools and reviewers are abstracted by their inputs, capabilities,
outcomes, and evidence. External tool/OS primitives, checker semantic honesty,
and reviewer accuracy remain explicit trust assumptions. Inventory,
construction, and tracking algorithms are modeled and tested through real
providers, not exempted by assuming their metadata is complete.

The central safety property is that publication requires admissible evidence for
every obligation required by the current scope and policy, bound to the current
publishable state. Cache reuse never authorizes a state that the reference
procedure would reject under the same modeled conditions. Failures, hazards, and
incomplete checks cannot become success. Successful-workflow witnesses and
independence cases prevent vacuous always-reject or always-run implementations.

### Production conformance

[Acceptance](#success-criteria).

Generated traces exercise shared production behavior in `loom-gate` and its
actual workflow/CLI consumers. The adapter translates actions and observations;
it does not reproduce planning, selection, aggregation, or authorization
algorithms. It compares relevant state and outcomes after each transition,
including attempted forbidden operations, rather than only replaying valid happy
paths.

Generated repositories exercise subject discovery, tracked observations, checked
Rust/Nix execution construction, generated provider descriptions, and source
origins. The real parse/resolve boundary and invalid-definition -> diagnostic ->
correction -> successful admission cycle participate in conformance; the adapter
does not inject already-admitted units or bypass the normal repair path. An
omitted subject remains a defect even if a verifier over the reduced universe
executes hermetically. Controlled external processes are fixtures subject to the
test-infrastructure rules in [Tests](tests.md); internal gate decisions are not
model-shaped mocks.

#### Portable replay regressions

[Acceptance](#model-and-implementation-conformance).

A fix for a confirmed model or conformance counterexample includes a portable,
checked-in replay regression through normal code/test changes under the existing
review policy. The regression preserves the relevant sequence, inputs or fault
conditions, and expected safety behavior. Repository fixtures and declared
execution resources make it reproducible without the original machine's paths,
transcripts, or local safety-history store. Fresh clones discover and cover it
through the existing verification graph without repeating the original deep
search; affectedness and evidence reuse retain their ordinary rules.

Counterexample detection and recording retain witnesses under
[Evidence's safety-history contract](evidence.md#retained-safety-evidence); they
do not generate or commit regression source automatically. The normal fix
workflow authors that source, and existing semantic review checks that the
regression addresses the confirmed defect rather than merely adding a fixture. A
checked-in replay is a verification input, not a transferred safety-history
record or a resolution token. Evidence continues to own
[counterexample resolution](evidence.md#confirmed-counterexamples-across-campaigns).

### Execution policy

[Acceptance](#success-criteria).

Model activities use
[Gate's deterministic verify flow](gate.md#deterministic-verify-lanes) and
spec-owned stage applicability. There is no extra conformance hook, gate tier,
or authorization route. Routine implementation feedback uses bounded, fixed-seed
simulation and conformance. Symbolic verification runs when its own
model/import/property/bound/checker inputs change, or consumes admissible
exact-input evidence under Evidence's admission policy.

Tools and solvers are pinned and provisioned ahead of routine verification.
Discovery and trace processing are batched, without a subprocess per criterion
or trace. Routine and explicitly requested deeper campaigns have finite resource
budgets. Shipped routine and deeper campaign definitions provide usable
defaults; ordinary callers need not configure seeds, bounds, or timeouts to
obtain the required verification. Defaults are explicit, versioned verification
inputs and are checked against coverage and cost requirements, not inferred from
a check running slowly. Results identify their tools, properties, bounds, seeds,
and inputs; timeouts, empty campaigns, and incomplete exploration do not count
as success.

### Pilot adoption and cost

[Acceptance](#success-criteria).

The pilot integrates the actual Loom and Wrix gate consumers, including their
real provider definitions and hook entrypoints. An independent consumer fixture
is useful regression coverage but is not a substitute for either integration.
Wrix-owned plumbing changes remain upstream work; a replacement shim or a
Loom-only demonstration does not complete this pilot.

Routine simulation and conformance are included in the complete fast-feedback
cost measured through [Pre-commit](pre-commit.md#fast-feedback-cost), not timed
in isolation. Both consumers need measured warm and cold evidence, including
affected result-cache misses, actual rebuild/iteration costs, and expensive work
avoided. Baseline tools/build artifacts, environments, revisions, campaigns, and
cache state are recorded so that a cache-hit-only demonstration cannot stand in
for affected execution. No fixed wall-clock adoption threshold is chosen before
profiling. Routine and deeper work still have explicit finite execution budgets
and participate through Gate's ordinary applicability rules; cost cannot
silently move required work to a later stage.

Adoption uses the shipped defaults and a small number of justified spec-owned
exceptions where needed, not a benchmark-only configuration that weakens
coverage. Users do not maintain a stage matrix, duplicated dependency lists, or
per-check timing knobs to obtain sound behavior and useful routine performance.
The pilot does not add general-purpose trusted providers for arbitrary non-Nix
projects; generic interfaces, existing Rust feedback, and necessary effectful
execution remain in scope.

### When to model

[Acceptance](#success-criteria).

Quint is justified when consequential behavior depends on sequences,
interleavings, retries, faults, or stale observations. A useful bounded
abstraction, meaningful properties, a real production bridge, and affordable
verification make that justification actionable. Straightforward parsing,
serialization, formatting, and pure rules normally need examples and properties,
not a state-machine model. Model necessity and semantic alignment require human
review; no keyword classifier or compiler can establish intent.

## Success Criteria

### Model and implementation conformance

<!-- prettier-ignore -->
- The whole-gate Quint model checks the owning gate, workflow, and hook
  contracts across every tier and stage, with explicit abstractions,
  assumptions, bounds, properties, and checker identity. Bounded success is
  reported as bounded evidence, not an unrestricted proof. [system?](nix run .#test-quint -- model)

<!-- prettier-ignore -->
- The optimized gate preserves reference per-obligation outcomes for stable
  valid deterministic inputs; effectful outcomes obey the modeled freshness and
  admissibility policy. Criteria with different stage applicability can share a
  unit without losing an eligible obligation. Selection, batching, concurrency,
  and reuse never authorize a state rejected by the reference under the same
  modeled conditions. [system?](nix run .#test-quint -- reference-equivalence)

<!-- prettier-ignore -->
- Generated repository/change fixtures exercise production inventory discovery,
  tracked input collection, actual Rust/Nix unit construction, generated JSON
  descriptions, and parse/resolve admission against the model. Invalid
  definitions produce actionable failure before dependent execution; correcting
  them uses the same admission path. Independence cases leave unrelated unit
  identities/results valid; required new subjects and resources invalidate the
  appropriate work rather than relying on hand-supplied correct input graphs.
  [system?](nix run .#test-quint -- input-generation)

<!-- prettier-ignore -->
- **Cache identity and admission.** The model covers absent/corrupt entries,
  definition/input/campaign changes, new obligations, and admitted/rejected Nix
  imports. It checks current signer trust for already-imported results,
  provenance/context mismatches, environment equivalence, and build-versus-result
  distinctions. Every required test executes or has admissible evidence of an
  equivalent execution. [system?](nix run .#test-quint -- cache-model)

<!-- prettier-ignore -->
- **Outcome history and projection.** Modeled failures invalidate prior passes;
  contradictory outcomes retain their reuse restrictions. Status and
  decomposition cannot promote historical passes after disqualifying same-commit
  admission changes or hide retained blockers. Reuse-disabled identities are not
  permanently failed criteria. [system?](nix run .#test-quint -- cache-model)

<!-- prettier-ignore -->
- **Freshness and attempt lifetime.** The model checks claim-specific freshness,
  explicit diagnostic reruns, same-attempt handoff, and mutation between
  validation and use. Failed/interrupted pushes end attempts; retries after
  process loss or unknown remote outcomes preserve eligible unit reuse without
  reviving ended authorization. [system?](nix run .#test-quint -- cache-model)

<!-- prettier-ignore -->
- **Durable shared history.** Modeled persistence/consolidation, transcript
  expiry, index rebuilding, and interrupted recovery preserve required safety
  facts and replay witnesses. Operator/integration checkouts share admitted
  observations, including concurrent updates. Missing required history cannot
  become empty-history success. [system?](nix run .#test-quint -- cache-model)

<!-- prettier-ignore -->
- **Cross-campaign counterexamples.** Confirmed violations block affected
  publication across routine/deeper campaign identities until current regression
  evidence resolves them. A cached routine pass cannot bypass that obligation.
  [system?](nix run .#test-quint -- cache-model)

<!-- prettier-ignore -->
- The model admits a valid path from discovery to publication and exercises
  forbidden attempts with expected rejection, including stale evidence,
  out-of-order completion, repeated actions, incomplete batches, skips, and
  changes during a run. [system?](nix run .#test-quint -- witnesses)

<!-- prettier-ignore -->
- Quint-generated traces drive actual shared gate logic and production
  dispatch/workflow seams, comparing selected targets, invocations, state
  projections, evidence, and authorization after each relevant transition.
  Automatic stage context comes through actual caller/hook paths, including
  marker-free publication and rehearsal, not an injected already-valid stage.
  Pending readiness uses real planner admission and scheduling; actual
  task-producer/dispatch paths distinguish criterion assignments from finding
  goals, including repair of malformed acceptance. External processes may be
  controlled fixtures; internal planning, aggregation, and authorization are
  not replaced by model-shaped mocks. [system?](nix run .#test-quint -- conformance)

<!-- prettier-ignore -->
- Conformance failures report the seed, model and input identities, trace, first
  divergent transition, and expected/observed results sufficiently to reproduce
  the failure; minimized confirmed counterexamples become regressions usable by
  routine verification without repeating the original deep search.
  Replay/resolution exercises the production path and current relevant inputs,
  rather than dismissing a witness solely because its campaign identity differs.
  [system?](nix run .#test-quint -- replay)

<!-- prettier-ignore -->
- Checked-in replay regressions participate in production discovery and routine
  verification from a fresh clone without the originating workspace's logs or
  safety history, and without rerunning the original deep search. Replay drives
  the relevant production behavior under the recorded conditions; counterexample
  detection and recording do not automatically generate or commit regression
  source. [system?](nix run .#test-quint -- replay)

- Existing semantic review rejects a fix for a confirmed counterexample that
  omits a portable checked-in replay regression or substitutes a case that does
  not address the defect. No separate approval or review path is introduced.
  [judge?](../tests/judges/quint-model-alignment.md)

<!-- prettier-ignore -->
- **Selection and readiness mutations.** Fault injection detects omitted
  obligations, deferral leaking to siblings sharing a verifier, untracked
  membership/absence dependencies, dropped batch results, skip-as-pass,
  absent-tool success counted as host coverage, and pending-probe admission or
  scheduling bypasses/probes-as-passes. It also detects unnecessary unrelated
  execution at the responsible model/provider/adapter/production boundary.
  [system?](nix run .#test-quint -- mutations)

<!-- prettier-ignore -->
- **Evidence-admission mutations.** Fault injection detects weakened cache
  keys/validation, stale or resurrected passes, restored reuse after conflicting
  outcomes, builds treated as results, imports treated as fresh executions, stale
  signer-trust decisions, and cache-source/trust changes erasing failure history.
  [system?](nix run .#test-quint -- mutations)

<!-- prettier-ignore -->
- **Publication mutations.** Fault injection detects wrong-scope publication,
  lost publication context downgraded to feedback, rehearsal coverage mistaken
  for authority, and attempt/outer-shortcut bypasses at the responsible boundary.
  [system?](nix run .#test-quint -- mutations)

<!-- prettier-ignore -->
- **Retained-history mutations.** Fault injection detects routine passes hiding
  confirmed deeper violations, witness loss across restarts/campaign changes,
  clone-local history hiding shared failures, lost concurrent updates, worker
  writes/forged associations, expiry/reindex erasure, and deletion before durable
  consolidation. It rejects compact records as passes, missing history as empty,
  and unchecked confirmation/resolution. [system?](nix run .#test-quint -- mutations)

<!-- prettier-ignore -->
- **Task and projection mutations.** Fault injection detects dropped ordinary
  references or conversion into remediation, repair goals requiring already-valid
  broken criteria, cached-status coverage despite current blockers, and reuse
  restrictions presented as permanent criterion failures.
  [system?](nix run .#test-quint -- mutations)

- Missing tools, invalid models, empty trace batches, adapter failures,
  incomplete exploration, and timeouts fail visibly; none count as completed
  model verification or implementation conformance.
  [test](quint_incomplete_verification_is_not_success)

- Semantic review checks correspondence between prose, model assumptions,
  properties, and implementation projections; an unchanged model's passing
  result is not evidence of alignment with changed English requirements.
  [judge?](../tests/judges/quint-model-alignment.md)

### Execution policy and cost

- Model, import, property, bound, or checker changes select both model
  verification and applicable implementation conformance; implementation,
  adapter, projection, or fixture changes select conformance and select model
  verification only when its own input identity changes. Relevant tool changes
  invalidate the activities that use them.
  [test?](quint_execution_triggers_follow_activity_inputs)

- Known-unrelated changes select neither model activity; unresolved discovery
  fails explicitly. A model-only edit selects Rust conformance even though the
  model is outside the Rust source tree.
  [test?](quint_model_changes_select_production_conformance)

<!-- prettier-ignore -->
- Routine implementation feedback selects bounded fixed-seed simulation and
  conformance through the existing stage-aware `loom gate verify` path, not a
  separate conformance hook. Unchanged symbolic checking is not repeated;
  changed model-verification inputs select symbolic checks or consume admissible
  exact-input evidence where policy allows reuse. [system?](nix run .#test-quint -- incremental)

<!-- prettier-ignore -->
- Model tools and solvers are pinned and provisioned ahead of routine runs;
  trace generation and verification-graph discovery are batched rather than
  spawning or downloading tools per criterion or trace. [system](nix run .#test-quint -- provisioning-and-batching)

<!-- prettier-ignore -->
- Cost evidence from actual verification/hook entrypoints covers unrelated,
  implementation-only, model-only, fixture, configuration, and full-publication
  changes with warm and cold inputs. It separates discovery, Nix
  evaluation/build, simulation, conformance, symbolic checking, and effectful
  work rather than using selected-test count as latency. [system?](nix run .#test-quint -- cost-evidence)

- Shipped routine and separately requested deeper campaigns provide explicit
  finite defaults without requiring per-user seeds, bounds, or timeout settings.
  The deeper campaign declares larger bounds or trace counts. Completing the
  declared bounded work may report bounded success; hitting a resource limit
  before that work completes reports incomplete verification. Slow execution
  does not silently reduce coverage or change applicability.
  [test](quint_campaign_budget_exhaustion_is_explicit)

<!-- prettier-ignore -->
- The pilot integrates actual Loom and Wrix consumers with their production
  providers and hook entrypoints, demonstrates avoided expensive unrelated
  executions, detects over-selection and under-selection defects, and reports
  end-to-end feedback costs against recorded baselines while preserving
  publication coverage. It uses shipped defaults with justified spec-owned
  exceptions, not coverage-reducing benchmark configuration. A passing isolated
  model, a synthetic second consumer, or unchanged over-broad production
  dispatch cannot substitute for adoption in both projects. [system?](nix run .#test-quint -- adoption)

- Verification-strategy review requires a rationale for consequential stateful
  workflows: a useful bounded model and production bridge, or an adequate
  alternative. Straightforward parsing, serialization, formatting, and pure
  rules do not acquire a mandatory Quint model merely because they have a spec.
  [judge?](../tests/judges/quint-strategy.md)

## Requirements

### Functional

1. **Independent oracle.** Derive required obligations independently of the
   optimized plan. Model the full procedure and rejected transitions; do not
   assume correct inventories, dependency lists, or cached target sets.
2. **Thin production adapter.** Exercise shared production providers and gate
   consumers. Record the first divergence and preserve reproducible regressions.
3. **Shared verification path.** Model activities participate through Gate's
   normal selection, execution, and evidence rules. Campaign identity and
   activity dependencies control reruns, not an unconditional second hook.
4. **Appropriate model use.** Require a justified abstraction and meaningful
   properties for consequential workflows; review correspondence with intent.

### Non-Functional

1. **Iteration cost.** Batch discovery and traces; measure cold/warm behavior
   and actual invocation costs. The pilot must improve real gate work, not only
   pass an isolated model.
2. **Reproducibility.** Identify tools, properties, bounds, seeds, inputs, and
   execution policy in every completed campaign and counterexample.
3. **Honest assurance.** Distinguish bounded exploration, conformance,
   hermeticity, inventory completeness, and semantic review.
4. **Consumer value.** Integrate both Loom and Wrix through the shared
   production APIs and actual hook paths. Small independent fixtures supplement
   those integrations; they do not replace them. Select and record concrete
   finite bounds and deeper limits during implementation profiling, constrained
   by the required witnesses, fault-detection coverage, and measured routine
   cost, rather than requiring user tuning or guessing a wall-clock threshold.

## Out of Scope

- Owning gate selection, evidence, or publication rules. The model verifies the
  contracts of the linked component owners rather than replacing them.
- Additional general-purpose trusted non-Nix provider backends. Deliver the
  actual Loom/Wrix integrations first without weakening shared admission rules
  or replacing generic APIs with repository-specific planners.
- A model for every spec, or a separate Quint-only gate/annotation tier.
- Proving arbitrary Rust or shell programs correct, modeling compiler/solver
  internals, or claiming unbounded correctness from bounded exploration.
- Treating hermetic execution as proof that a verifier examines the right
  subjects or checks the intended requirement.
- Formalizing an LLM's semantic correctness; model its evidence and
  authorization boundary, not the truth of every judgment.
- Broad adoption of Lean, Verus, Kani, or other proof systems in this pilot.

## References

- [Quint](https://github.com/quint-co/quint): executable specifications,
  simulation, and model checking.
- [Quint Connect](https://github.com/quint-co/quint-connect):
  model-to-implementation trace execution and comparison.
- [Build Systems à la Carte](https://www.microsoft.com/en-us/research/wp-content/uploads/2018/03/build-systems-final.pdf):
  task dependencies, scheduling/rebuilding separation, and relative minimality.
- [Forward Build Systems, Formally](https://ndmitchell.com/downloads/paper-forward_build_systems_formally-17_jan_2022.pdf):
  reference-equivalence modeling; its correct-tracing assumption and omission of
  directories are not a proof of Loom's input boundary.
- [Skyframe](https://bazel.build/reference/skyframe): tracked input requests,
  directory membership, and incremental evaluation.

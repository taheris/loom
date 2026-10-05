# Verification evidence

Admits reusable results and preserves failure, counterexample, freshness,
provenance, and recovery facts independently of cached status.

## Problem Statement

Admits reusable results and preserves failure, counterexample, freshness,
provenance, and recovery facts independently of cached status. This package is
one contract owner, not a new crate or command tree.

## Architecture

Inputs, outputs, and trust boundaries are stated in the contracts below. Related
owners: [verify](verify.md), [gate](gate.md), [events](events.md),
[harness](harness.md), [simulation](simulation.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Cache and evidence reuse

[Acceptance](#execution-and-admissible-evidence).

Every required obligation is discharged by fresh execution or admissible
evidence of an equivalent execution. Build reuse is not result reuse: a cached
library or test executable avoids compilation but does not establish a pass. A
successful Nix unit that actually verifies targets may supply result evidence
subject to provenance and policy.

Execution identity covers requested verifiers/targets, checker/artifact
identities, observed inputs, fixtures, effective invocation and cwd, campaign
parameters, relevant environment/configuration, and freshness policy. Traces
include membership and absence observations. Dependency traces and result
references extend normal gate evidence and Nix artifacts, not a parallel receipt
or general-purpose cache service. SQLite status/lookup indexes remain
rebuildable views, not authority.

Valid providers without admissible prior results execute and collect missing
observations; a missing trace is not an empty dependency set. This cold path is
not a substitute for admitting a provider contract. Malformed contracts fail
admission. Current contracts and inventory, never cached targets, determine
required coverage. Unchanged unit evidence can survive an unrelated edit;
whole-scope authorization must still match the current request and state.
Changes between validation and use invalidate affected evidence or
authorization.

#### Nix-cache result admission

[Acceptance](#execution-and-admissible-evidence).

Results imported from trusted Nix caches are eligible verification evidence, not
limited to builds performed locally. Gate admits them only for an admitted
verification unit that actually verified the required targets, with matching
execution identity and environment assumptions and accepted provenance binding
the result to that execution. A store path, successful substitution, or cached
ordinary build does not by itself establish a verifier pass. A valid signature
authenticates its signed artifact; it does not by itself establish semantic
coverage or environment equivalence.

Cache signer trust comes from Nix's effective configured signing-key trust for
the verification context. Loom maintains no separate cache/signer allowlist and
does not accept trust roots declared by the imported artifact itself. Store
acceptance without authenticated signer provenance is not, by itself, evidence
of that provenance. Gate separately checks the unit, execution context, and
result eligibility; Nix trust does not replace those checks.

Admission requires provenance valid under current signing-key trust; store
acceptance under an earlier policy is not sufficient. Trust changes require
re-admission of affected imported evidence, including artifacts already present
in the store. Changing signing trust neither creates fresh execution nor erases
the recorded history of an unchanged execution identity.

Imported artifacts enter the same parse-and-admit boundary as other evidence;
external data cannot deserialize directly into trusted evidence or publication
authority. Imports remain subject to current inventory, freshness, recorded
failure history, and publication-attempt rules. Fetching or reimporting a result
is not fresh verification and cannot satisfy exact-target execution intent or
restore a disqualified pass. Changing where a result is cached does not create a
new execution identity or erase its recorded history.

Missing or inadmissible imported evidence leaves required work to the ordinary
execution path; it does not justify skipping or weaken provider-contract
admission. Eligible unit evidence can be reused without repeating verification,
but authorization is still derived for the current request.

#### Failure invalidation

[Acceptance](#execution-and-admissible-evidence).

A trusted fresh verifier failure disqualifies earlier passes for that execution
identity, including dependent publication markers. This applies across runs and
includes exact-target diagnostics. Reloading or reimporting an older pass does
not make it new evidence; status insertion order is not authority.

Once trusted executions disagree between pass and failure, result reuse stays
disabled for that identity even after a later fresh pass. Required checks remain
executable, and fresh outcomes enter the normal gate procedure. This is neither
a permanent failure verdict nor an automatic retry policy. Independent
admissible results and build/image caches remain usable. A different admitted
identity follows ordinary rules without erasing the old identity's history or
bypassing the claim-level counterexample rule below.

Inability to start or obtain an outcome remains distinct from an observed
verifier failure. Neither may be reported as a pass or weaken failure, skip, or
freshness rules.

#### Confirmed counterexamples across campaigns

[Acceptance](#execution-and-admissible-evidence).

A confirmed counterexample to an applicable claim blocks publication of the
affected state even if a routine campaign passes or has reusable passing
results. A deeper campaign's different bounds, seeds, or execution identity do
not isolate that knowledge from ordinary verification. Confirmation binds the
witness to the violated claim and relevant model/implementation inputs and
assumptions through the normal evidence-admission boundary; an unchecked report,
tool failure, or timeout is not a confirmed counterexample. Required incomplete
or failed checks still follow their ordinary failure rules.

The confirmed witness becomes a regression obligation through the existing
planner and evidence path. Replay the witness against the applicable current
state rather than require the full deep search again merely to recover the same
defect. Switching campaigns, changing cache keys, reimporting passes, or
restarting cannot discard an unresolved counterexample. Model or implementation
changes require re-evaluating the regression, not assuming a new identity fixed
it. Resolution needs admissible evidence addressing the witness under current
requirements and assumptions; a routine pass that never exercises it is not
resolution.

Counterexample admission invalidates dependent authorization, including earlier
receipts and markers. Preserve the witness and its resolution history in normal
gate evidence, not only the latest status row. Unrelated claims and otherwise
admissible unit/build evidence remain usable. Resolving the counterexample does
not erase an execution identity's separate pass/fail disagreement history.

#### Retained safety evidence

[Acceptance](#execution-and-admissible-evidence).

Evidence retains compact canonical safety records and required replay witnesses
independently of
[ordinary event-log expiry](events.md#persisted-logs-and-replay). They preserve
the admitted outcome facts needed for failure invalidation and pass/fail
disagreement, plus confirmed counterexamples, their claim/input attribution, and
resolution history. Required provenance and replay inputs remain available; a
path into an expired transcript or a latest-status scalar is not sufficient.
Transcript age, restart, index rebuilding, or cache import cannot reset these
facts or remove a regression obligation.

These records belong to existing typed Gate evidence handling, not a second
result cache, authorization route, or mutable summary with independent
authority. Compaction may consolidate representation, but cannot change the
safety facts or their admission meaning. Recording and consolidation must
preserve them across interruption, including before a complete `GateRun` end
event exists. Cleanup must not remove the last usable copy before its retained
replacement and required witnesses are durable. Missing, corrupt, or incomplete
required safety evidence blocks dependent reuse and authorization with
actionable diagnostics; it is not an empty history. A fresh pass cannot silently
repair lost disagreement or counterexample knowledge.

Ordinary transcripts still expire automatically; users need not disable
retention or retain every full log indefinitely. Compact safety records do not
replace the successful run evidence required by a receipt, extend an attempt's
lifetime, or establish a current pass. Missing positive evidence follows
ordinary execution/admission rules. Independent admissible evidence remains
reusable.

#### Claim-specific freshness

[Acceptance](#execution-and-admissible-evidence).

Freshness follows the claim, not its tier or use of containers. Artifact and
controlled-integration evidence may survive gate runs when complete execution
identity and admitted environment assumptions match, including relevant
launcher/harness code, images, fixtures, configuration, and campaign inputs.
Image/version strings alone do not establish equivalence. A new gate run or
container use alone does not invalidate evidence.

Live-service, actual Git publication-state, and explicit current-host-readiness
claims require fresh observations. Artifact smoke coverage does not implicitly
claim publisher-host readiness. Refreshing mutable facts does not force
independent artifact checks to rerun; authorization must cover current
obligations and state. [Exact-target intent](gate.md#scope-flags) separately
requires fresh verification without invalidating reusable build/image artifacts.

### Relocated claims

[Acceptance](#relocated-claims-1).

Relabeling a claim cannot erase known failures, disagreement, or confirmed
counterexamples. Retained safety records preserve explicit linkage to the
relocated claim and its relevant execution identities and assumptions. Positive
evidence must be re-admitted under current contracts, not automatically
transferred as authority; current claim/identity resolution cannot treat
unavailable required linkage as empty safety history.

## Success Criteria

### Execution and admissible evidence

- Reuse requires matching execution identity, observed inputs, campaign
  parameters, environment, and freshness policy plus trusted successful
  evidence. Missing, corrupted, incomplete, stale, failed, skipped, or untrusted
  artifacts cannot discharge an obligation.
  [test?](quint_reuse_requires_admissible_matching_evidence)

<!-- prettier-ignore -->
- The production Nix-cache import and Gate admission path reuses a trusted
  verification-unit result with matching execution context without rerunning the
  verifier. Signer trust follows effective Nix configuration without a second
  Loom allowlist; signing-trust changes require re-evaluating admission even
  when the artifact remains in the store. Store acceptance alone cannot
  substitute for authenticated provenance. Build-only, untrusted, incomplete,
  mismatched, stale, or disqualified imports cannot discharge obligations;
  reimport, a different cache source, or changed signing trust cannot
  manufacture fresh execution or reset failure history. Current request
  authorization remains separate from accepting the imported unit result.
  [system?](nix run .#test-quint -- nix-cache-evidence)

<!-- prettier-ignore -->
- A trusted fresh verifier failure disqualifies earlier passes for that same
  execution identity on subsequent verification and publication, including
  marker reuse. Reloading or reimporting those passes cannot restore their
  eligibility; independent, otherwise admissible evidence remains usable.
  [system?](nix run .#test-quint -- failure-invalidates-prior-passes)

<!-- prettier-ignore -->
- Trusted pass/fail disagreement disables verification-result reuse for that
  execution identity, even after a later fresh pass. Required checks remain
  executable under ordinary gate rules; build/image reuse and independent
  admissible evidence remain available. Different admitted execution identities
  follow the ordinary reuse policy without resetting the conflicted identity or
  bypassing an unresolved applicable counterexample. [system?](nix run .#test-quint -- conflicting-outcomes-disable-reuse)

<!-- prettier-ignore -->
- A confirmed deeper-campaign counterexample blocks the affected claim's
  publication despite routine passes at a different campaign identity, including
  cached passes and earlier receipts/markers. Production admission retains a
  replayable regression across restarts and campaign/cache changes; current
  resolution addresses the witness without requiring the original deep search.
  Timeouts or unchecked reports are not confirmed counterexamples, and unrelated
  admissible evidence remains reusable. [system?](nix run .#test-quint -- counterexample-invalidates-routine-evidence)

<!-- prettier-ignore -->
- Compact canonical safety records and required replay witnesses survive normal
  transcript expiry, restart, and index rebuilding. Production admission still
  rejects disqualified passes and unresolved counterexample bypasses after cache
  reimport, while independent evidence remains reusable and expired ordinary
  transcripts are removed without disabling retention. [system?](nix run .#test-quint -- evidence-retention)

<!-- prettier-ignore -->
- Interrupted safety-record persistence or consolidation cannot erase admitted
  facts or required witnesses, including observations before a run's end event.
  Missing, corrupt, or incomplete required safety evidence produces actionable
  rejection of dependent reuse and authorization, not empty-history success or
  repair by a fresh pass alone. Retained facts are not replacement positive
  evidence for a receipt. [system?](nix run .#test-quint -- evidence-retention-recovery)

<!-- prettier-ignore -->
- Artifact and controlled system-test evidence can be reused across gate runs
  when complete admitted execution inputs and environment assumptions match.
  Container use alone does not force execution; image-only or version-only
  matches cannot substitute for omitted relevant launcher, harness, fixture,
  configuration, or runtime context. [system?](nix run .#test-quint -- system-artifact-reuse)

<!-- prettier-ignore -->
- Claims about current external-service or Git publication state, and explicit
  current-host-readiness claims, obtain fresh observations even when source
  inputs are unchanged. Those observations do not force unrelated artifact
  checks to rerun when their evidence remains admissible; old whole-scope
  authorization is not carried forward. [system?](nix run .#test-quint -- live-state-freshness)

<!-- prettier-ignore -->
- Cached build artifacts are not verification results. Only evidence from an
  execution that actually verified the requested targets can discharge them; a
  successful build or cached status scalar cannot substitute for that evidence.
  [system?](nix run .#test-quint -- build-versus-result-cache)

### Status cache

- `.loom/cache.db` is created on first `open` when the path is missing
  [test](open_creates_db_file_when_missing)

- A criterion-evidence `CacheRow` round-trips through sqlite preserving every
  field, including typed `(SpecLabel, CriterionId)` identity and the current
  annotation snapshot [test](round_trip_through_sqlite_preserves_every_field)

- The `row_for` helper writes a row that round-trips through the unified cache
  [test](row_for_helper_writes_round_trip_row)

- Verifier evidence predating the current evidence version is invalidated once
  because historical passing rows may represent ignored tests or invalid
  producer reports; subsequent cache opens preserve new evidence and unrelated
  cache state [test](unversioned_verifier_evidence_is_invalidated_once)

- Report rendered from on-disk rows summarises pass/fail per tier
  [test](render_report_reads_from_disk_and_summarises_per_tier)

- Broken-annotation entries in the report come from integrity findings, not from
  the cache file itself
  [test](broken_annotations_in_report_come_from_integrity_findings)

- **Cache render <500ms — sqlite path.** The report renders in <500ms on a
  2000-row corpus when read from sqlite (hard target from _Status cache_)
  [test](render_under_500ms_on_2000_row_corpus)

- **Cache render <500ms — in-memory path.** Same <500ms target holds for the
  in-memory `render_from_rows` path
  [test](render_from_rows_under_500ms_on_2000_row_corpus)

### Relocated claims

<!-- prettier-ignore -->
- Claim relocation preserves explicit negative-history and witness linkage,
  while positive evidence is re-admitted under current obligations and cannot
  gain authority merely from relabeling. [system?](nix run .#test-quint -- claim-relocation-evidence)

## Requirements

#### Status cache

[Acceptance](#status-cache).

`loom gate status` reads criterion evidence from the unified `.loom/cache.db`
cache and prints a fast report. (Bare `loom gate` shows the subcommand help —
see _Commands_ above.) Every subcommand that runs verifiers or the LLM rubric
writes to the cache as it runs — `loom gate verify`, `loom gate review`,
`loom gate audit`, the tier subcommands (`check` / `test` / `system` / `judge` /
`rubric`), and `loom gate mint` (via its embedded verify and rubric walks).
There is no separate `.loom/gate-cache.sqlite`.

**Cache contents per criterion:**

- typed `CriterionId` (requirement identity) and current annotation snapshot
- last-run timestamp and commit hash
- pass / fail / skipped verdict (`skipped` covers scope-filter exclusion and
  verifier-reported prerequisite gaps via exit 77)
- evidence string from the verifier's JSON output

**Cache schema** is part of `.loom/cache.db` in [Harness](harness.md). One row
per criterion, indexed by typed `(SpecLabel, CriterionId)`. If the current spec
file's annotation differs from the cached annotation for the same criterion id,
todo renders `EvidenceState::StaleAnnotation` rather than reusing stale pass
evidence.

**Report contents** when `loom gate status` runs:

- per-spec criterion counts: total, annotated, un-annotated
- last-run summary per tier: when, pass/fail counts, currently-failing criteria
- annotation health: broken annotations (target doesn't resolve), stale runs
  (cache older than N days)

**Hard target:** report renders in <500ms on a corpus of arbitrary size. A
self-test asserts this — the cache implementation, not the corpus, is what
determines the latency.

### Functional

- Status cache schema: per-criterion row with annotation target, last-run
  timestamp, commit hash, verdict (pass / fail / skipped), evidence string
  - Status cache writes on every verifier invocation; `loom gate status` reads
    it for the report. Bare `loom gate` follows the command contract in
    [Gate — Commands](gate.md#commands) and prints help without a cache read

## Out of Scope

- Evidence admission does not independently authorize publication. SQLite status
  is not canonical safety history; ordinary transcript rendering belongs to
  Events.

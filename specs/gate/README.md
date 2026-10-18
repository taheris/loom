# Whole-gate bounded reference model

`model.qnt` composes the contracts of [Gate](../gate.md),
[Verify](../verify.md), and [Evidence](../evidence.md). It is the reference for
production conformance, not an implementation of those production components.
`witness.qnt` supplies portable executable sequences through the model's own
transitions. Neither file supplies admitted production evidence.

## Abstraction and trust boundary

Eight repository subjects represent two shared check obligations, a test and a
system sharing another unit, a live system, a judge, pending test readiness
sharing the test unit, and a project hook. The universe includes membership and
absence observations. Discovery reads current subjects, not previous targets;
planning applies each criterion's policy before execution sharing. Readiness has
a different execution purpose and never becomes an ordinary criterion pass. An
absent pending definition has no probe; an absent ordinary definition or invalid
offered definition prevents admission. Structural success precedes behavioral
discovery/build/launch. Diff hooks precede the annotation lane; modified-tree,
unavailable, skip and timeout outcomes cannot supply coverage.

The fixed unit assignment is a bounded supported dependency graph, not an
assumption that a production provider's inventory or tracing is complete.
Generated-repository conformance must compare real discovery and construction
against it. Integer input and definition versions abstract canonical checked
execution inputs, tools, invocation, resources and environments. Membership,
absence, campaign and purpose participate in the manifest. Claim/policy edits,
labels, signer trust, history and attempt liveness do not salt execution
identity. Changed bindings still require current admission. Builds remain
separate from result facts.

For deterministic units, input version 1 fails and versions 0 and 2 pass. The
full reference evaluates every current applicable subject against that oracle;
it does not consult selected targets, reusable results or the inventory cache.
Effectful outcomes are external observations and need not agree across reruns.
Their reference projection consumes current-manifest observations, not the last
completion of an unrelated or stale execution. Both paths consult admitted
claim-level witnesses. `referenceEvidence` independently checks raw facts for
publication and cache invariants; it does not invoke the optimizer's admission
predicate. Missing facts mean required work, not an assumed reference execution.

External checker honesty, reviewer accuracy, Nix signature/store cryptography,
canonical manifest hashing, witness content addressing, OS handle liveness,
checkout association and locked durable append/checkpoint primitives are trust
assumptions. Boolean artifact properties represent outcomes of these external
checks, **not** accepted artifacts or publication permission. Import performs
current admission separately from execution; signer trust can change after
import. Conformance must exercise the real primitives and parse/admit boundary,
not replace them with these abstract outcomes. No cryptographic or filesystem
correctness is proved here.

The shared journal has ordered attributed events, a separately durable
checkpoint, immutable witness identities, a rebuildable index and independent
expiring transcripts. Recording is atomic at the admitted append boundary;
interleavings through two associated checkouts preserve both updates.
Interrupted writes make history unavailable until recovery from retained
complete sources. Witness loss and missing relocation linkage remain
unavailable, never empty history. Consolidation must precede deletion. Direct
untrusted history writes are rejected. Journal facts cannot substitute for
positive result evidence.

Confirmation requires an observed violation; timeout is not confirmation.
Resolution requires a fixed current input, a current fresh pass and a successful
witness replay, not merely a changed campaign or routine pass. A relocated claim
keeps its safety linkage. Pass/fail disagreement permanently disables reuse for
that manifest, but permits fresh passing execution and independent reuse.

Attempts are clone-local, live and single-push. An unsuccessful/interrupted or
unknown push and process loss end the attempt; the remote may nevertheless have
changed. Retry preserves independent execution facts, not the old receipt or
freshness-sensitive results. Launched tasks retain their original attempt even
if they finish after retry. Manual hooks establish publication applicability
without a marker; rehearsal requests coverage without authority. Nested hooks,
exact-target diagnostics, worker feedback, tree inspection and invalid handoffs
cannot authorize publication. Resolved criterion assignments and finding-driven
repair goals have separate admission; bad ordinary references cannot fall back
to repair, while malformed acceptance can still be repaired through a finding.

## Properties and witnesses

- `referenceEquivalence`: current-snapshot applicability agrees for every
  subject; settled optimized outcomes agree with the independent reference.
- `cacheSafety`: admitted verification/readiness coverage has independent
  current-manifest evidence, required safety history and no unresolved blocker.
- `publicationSafety`: each observed successful push agrees with the independent
  full-obligation authorization check at the validation/use boundary.

The 81 named tests include positive driver/manual publication, worker feedback,
all six stages, all four tiers and the hook lane, independent reuse, cold work,
shared deferral/order permutations, policy-only edits, corrected admission,
readiness, imports and trust removal/restoration, diagnostic freshness,
pass/fail/pass disagreement, ended attempts and unknown remote outcomes,
concurrent/stale completion, journal consolidation/expiry/restart,
cross-checkout history, witness replay/resolution and relocation. Rejected
attempts are observable transitions, including early publication, late-barrier
launch, untrusted writes and deletion before durable consolidation. There is no
always-reject, always-reuse or always-run success substitute.

`cacheStaleEffectCompletionDoesNotChangeCurrentResultTest` preserves the
out-of-order old-effect completion sequence that would otherwise corrupt a
unit-wide last-outcome oracle. The regression uses only checked-in model inputs.
Neither counterexample recording nor this harness generates regression source.

## Campaigns and assurance

`campaign.json` pins finite defaults and the checker identity. Routine and
explicit deeper campaigns run through the existing `test-quint` acceptance app;
there is no extra operational hook or publication route.

| Work     | Seeds                                           | Traces/seed | Steps/trace | Randomized test samples | Guard/checker invocation |
| -------- | ----------------------------------------------- | ----------- | ----------- | ----------------------- | ------------------------ |
| Routine  | 42, 20250308                                    | 64          | 80          | 32                      | 120 seconds              |
| Deeper   | routine + 314159, 271828                        | 256         | 160         | 128                     | 240 seconds              |
| Symbolic | exhaustive within declared initial states/bound | n/a         | 1           | n/a                     | 180 seconds              |

Every simulation trace must contain exactly the declared steps plus its initial
state. Every named test must complete, and every declared trace must be present;
short/empty output, checker failure and resource interruption are not bounded
success. The guard applies separately to each test batch, simulation batch and
symbolic invocation. Seeds, counts, bounds, tool identities and logical input
hashes appear in the receipt. Nondeterministic test cases use the declared
sample count; deterministic tests run once. They are not claimed as exhaustive
searches.

Apalache checks all transitions for one step from `symbolicInit`: either cold
initialization or a reachable sealed two-subject shared-unit state. The latter's
exact reachability through ordinary discovery/execution/review is an executable
witness, not an adapter-supplied trusted state. This frontier exercises reuse
and publication directly rather than spending the entire small symbolic bound on
setup. Longer histories, all tiers and full publication use the 81 tests and
fixed-seed 80/160-step simulation. This is explicitly **not** exhaustive
symbolic checking of all long whole-gate interleavings. Deeper symbolic
experiments that exceeded their guard were incomplete and are not receipts;
increasing exploration requires changing the versioned campaign and
re-profiling, never silently shortening a requested run.

Initial Linux x86_64 profiling with provisioned tools and no result cache
measured about one minute for the model scenario (routine tests/simulation plus
the symbolic frontier), and about 80 seconds for the harness's successful
campaign and injected failure cases. A 30-second routine guard interrupted work
when run alongside the full native suite; the shipped guard retains identical
coverage with 120-second headroom (240 for deeper work). The interrupted trial
was rejected, not counted as success. These are model-only costs, not Loom/Wrix
hook baselines or pilot-adoption evidence. The existing conformance,
input-generation, replay, mutation, incremental, cost and adoption scenarios
remain unimplemented until their production bridges land. Bounded model success
is neither implementation conformance, verifier honesty, human-intent alignment
nor an unbounded proof.

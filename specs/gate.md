# Loom Gate

The quality gate. Decides whether code is good enough to ship.

The umbrella concept covering the gate stages (plan / worker self-
check / per-bead integration / stabilization / push / standing safety
net) and one command tree (`loom gate <subcommand>`).
`loom gate verify` runs deterministic verifiers; `loom gate review`
runs the LLM rubric (inspection-only); `loom gate mint` materializes
molecule-deferred or tree-sweep findings into bd remediation work —
the only subcommand that mutates bd state. Distinct from the *Verdict Gate* execution layer
in [harness.md](harness.md) — that section owns the per-bead
mechanics that wrap the gate; this spec owns the rubric, the
invariants, the lanes, and the stages.

## Problem Statement

Loom's review machinery has multiple participants: a verdict gate
in [harness.md](harness.md) (worker, per-bead integration, and
push-range scoped), style rules in
[`docs/style-rules.md`](../docs/style-rules.md) (mechanical
lints), test strategy in [tests.md](tests.md). Each
carries part of the load. The gaps *between* them — cross-file
incoherence, multi-component contracts no individual bead owns,
omissions where no PR is the natural owner of the integration —
are structurally invisible without a consolidated review surface.

Omissions are the dominant failure mode in autonomous development —
more common than incoherence, more common than systematic errors.
File-scoped review detects none of the cross-file incoherence.
Coherence-only or file-scoped gates structurally cannot catch the
dominant failure modes.

This spec gives one place one responsibility: catch divergences
before they ship.

## Invariants — what must never happen

The five failure classes the gate guarantees against. These are the
gate's reason for existing; everything below them is mechanism.

1. **A spec claim is false in the code.** If a spec says X must
   happen, the implementation must make X happen. If a spec bans
   Y, the implementation must not contain Y. Includes
   multi-component contracts: parts {a, b, c} of a lifecycle
   either all land in the implementation, or the unfinished parts
   have a bonded successor doing the remaining work.

2. **A passing verifier is dishonest.** A deterministic verifier
   (`[check]`, `[test]`, or `[system]`) that asserts a tautology,
   mocks the thing it claims to test, or passes for the wrong reason
   is itself a divergence — the spec claim it cites is in fact
   unchecked. The gate distinguishes honest from dishonest
   verifiers; *all tests pass* is not synonymous with *the spec is
   enforced*.

3. **A template directs agents toward spec-contradicting behaviour.**
   Planning, decomposition, and review templates are themselves
   system artefacts. They must operate consistently with the specs
   they drive — a template whose instructions contradict its spec
   produces cascading damage as the agent follows the template
   literally instead of the contract.

4. **A divergence sits in the working tree undetected, regardless of
   whether any merge is in flight.** Finite-diff review can only see
   what's in the diff. Cross-file gaps, contracts orphaned across
   multiple PRs, pre-existing violations that predate current rules —
   none of these surface at merge time. Conformance is a property of
   the *current* code-spec pair, not a historical artefact of past
   approvals.

5. **A load-bearing invariant is silently contradicted.** Five
   invariant categories: architectural decisions, data-structure
   choices, explicit constraints, non-functional requirements, and
   out-of-scope items. A change that contradicts any such invariant —
   in code or in a sibling spec — must surface, never slip. *Not* a
   hard reject — clashes require human judgement (see Lanes, below).

## Dimensions

The gate evaluates code on three dimensions, all together. Failure on
any one is a flag.

- **Conformance** — for every claim in the spec, there is a true code
  path that makes it real.
- **Style** — the implementation follows the consumer's code-style
  rules (conventionally consolidated in a style-rules document such as
  `docs/style-rules.md`, organised by language- or domain-specific
  family).
- **Test quality** — the tests follow the consumer's test-quality
  rules (typically in the same document); verifiers actually verify
  what they claim.

The specific rule families, their prefixes, and the path of the
style-rules document are consumer-defined. The gate evaluates against
whatever rules the consumer specifies; it does not impose a
particular taxonomy.

These three dimensions are not separable concerns; they are aspects
of the same binary question: *is the code good enough to ship?* They
live in one gate by design — fragmenting them produced the failure
pattern this spec exists to prevent.

## Lanes

The gate has two response paths. The choice is dictated by the kind of
failure detected, not by stage or scope.

- **Hard fail (rule violation).** Code breaks an entry in the
  consumer's style-rules document, or a deterministic verifier
  (`[check]`, `[test]`, or `[system]`) that asserts a specific
  behavioural claim returns failure. There is no legitimate
  "keep this on top" path. The gate fails the check; per-stage
  recovery (same-bead recovery during worker/per-bead integration,
  push refusal plus remediation at push, remediation bead at standing)
  drives the response, all converging on *fix the code*.

- **Clarify (invariant clash).** Code (or a proposed spec change)
  contradicts a load-bearing invariant in a spec — one of the five
  categories from Invariant 5 above. The right path requires human
  judgement, framed by the *three-paths principle*:

  1. **Preserve the invariant** — rework the change so the invariant
     still holds.
  2. **Keep the change on top of the invariant** inelegantly, with the
     debt recorded in the spec or notes.
  3. **Change the invariant** — update the spec and plan follow-up
     work to realign code.

  The three-paths principle is *guidance, not a rigid template*. A
  given clash may need fewer or differently-framed options, each
  phrased in terms concrete to the clash.

  Gate raises `loom:clarify` per the *Options Format Contract*
  (defined in [Options Format Contract](#options-format-contract)
  below) and waits for `loom inbox` resolution.

## Commands

The gate is one umbrella command, `loom gate`, with subcommands
selecting what kind of inspection or act path runs:

| Command | Kind | Purpose |
|---|---|---|
| **`loom gate`** | Help | Prints `loom gate --help` — the subcommand list with one-line descriptions. No verifiers run, no cache read. |
| **`loom gate status`** | Status | Reads cached results for an explicit scope and prints a fast status report — no verifiers run. See *Status cache* for the hard latency target. |
| **`loom gate audit`** | All, inspection | Runs `verify` then `review` for an explicit `--diff` or `--tree` scope. Inspection composition only: no bd writes and no marker mint. The act path is `mint`. |
| **`loom gate verify`** | Deterministic | Runs deterministic gate lanes for an explicit scope. Diff scope includes both the project pre-commit lane and spec annotations; file/tree scope is spec-annotation only. |
| **`loom gate check`** | Deterministic, one tier | Runs only `[check]`-tier spec annotations for an explicit scope or exact `--target`. |
| **`loom gate test`** | Deterministic, one tier | Runs only `[test]`-tier spec annotations for an explicit scope or exact `--target`, batched into one runner subprocess per invocation. |
| **`loom gate system`** | Deterministic, one tier | Runs only `[system]`-tier spec annotations for an explicit scope or exact `--target`. Slow by default; used by `verify --tree`, CI, standing sweeps, or an operator's explicit `system` invocation. |
| **`loom gate review`** | LLM judge, inspection | Runs the LLM rubric for an explicit `--diff` or `--tree` scope. Inspection-only: emits `LOOM_FINDING:` records + terminal marker to stdout, makes no bd writes, and does not mint a marker. Push-eligible review consumes a typed `VerifiedScope` for the same resolved content/scope rather than a scalar exit-code flag. |
| **`loom gate judge`** | LLM judge, one lane | Runs criterion-attached `[judge]` verifiers for an explicit scope or exact `--target`; skips the rubric walk. Inspection-only, like `review`. |
| **`loom gate rubric`** | LLM judge, one lane | Runs only the rubric walk for an explicit `--diff` or `--tree` scope; skips criterion-attached judges. Inspection-only, like `review`. |
| **`loom gate mint`** | Act | Materializes findings into bd work. `loom gate mint -m/--molecule <id>` promotes that molecule's deferred remediation batches after original work drains; `loom gate mint --tree` runs the standing safety-net sweep and creates or updates ready remediation batches under one active work epic when actionable findings remain. `mint` has no per-bead, diff, file, spec-filter, or target surface. Clarify-route findings still materialize as one `loom:clarify` bead per finding so each carries one `## Options — …` block. See [*Findings and Minting*](#findings-and-minting). |
| **`loom gate verify-marker`** | Trust check | Reads `.loom/marker.json`, validates the current workspace fingerprint, and exits 0 iff the marker is well-formed and current. Diagnostic use on the CLI remains valid. The `pre-push-checks` wrapper performs the hook-coverage validation defined in *Marker* before short-circuiting any wrapped command; `verify-marker` is not registered as a standalone prek hook. |

Spec-specific target discovery is outside the gate command tree:
`loom spec <label> --targets` prints annotation targets for a spec,
optionally narrowed with `--tier <tier>`; `--plain` prints exact target
strings one per line for piping into `loom gate <tier> --target`.

### Scope flags

Gate subcommands do not infer a default verification scope. An
inspection subcommand invoked without an explicit scope or exact
`--target` prints that subcommand's help and runs no verifier, review,
cache lookup, bd write, or marker check. This includes bare
`loom gate verify`: users must choose what trust surface they are
asking about.

The scope flag defines the **input set** — the files the gate is being
asked about. A verifier whose inputs the gate can derive runs iff
those inputs intersect the input set (see *Verifier inputs* below); a
verifier whose inputs cannot be derived runs conservatively for the
eligible tier. An undeterminable input set is never grounds to skip an
eligible `[check]` or `[test]` verifier.

| Flag | Valid subcommands | Input set / meaning | Typical caller |
|---|---|---|---|
| `--files <paths...>` | `verify`, `check`, `test`, `system`, `judge`, `status` | Explicit file list. `verify --files` runs spec annotations only: eligible `[check]` and `[test]` targets whose inputs intersect the file set, plus conservative unknown-input targets. | pre-commit hook (`loom gate verify --files <staged files>`); local debugging |
| `--diff <range>` | `verify`, tier commands, `review`, `rubric`, `judge`, `audit`, `status` | `git diff <range> --name-only`, resolved to concrete base/head commits when the run is trust-bearing. | worker self-check, driver per-bead post-integration verify, push-range verify/review, CI scoped to a PR |
| `--tree` | `verify`, tier commands, `review`, `rubric`, `judge`, `audit`, `status`, and `mint` | Every file in the workspace. `verify --tree` runs `[check]`, `[test]`, and `[system]` spec annotations. | standing safety net, nightly CI, on-demand full inspection |
| `--target <annotation target>` | `verify`, `check`, `test`, `system`, `judge` | Exact annotation-target match. No project hook lane runs. | operator repeats one known target discovered via `loom spec <label> --targets` |
| `-m`, `--molecule <id>` | `mint` only | The molecule's deferred finding set. | stabilization promotion |

`--target` is mutually exclusive with `--files`, `--diff`, and
`--tree`. Matching is exact against the annotation target string after
markdown parsing; no substring search, shell splitting, globbing, or
positional selector fallback exists. Zero matches fail loudly. A target
that matches annotations in multiple tiers fails on `verify --target`
and suggests the tier-specific subcommand; multiple criteria sharing
the same target inside one tier are accepted and run as one target
selection.

`--bead` is not a deterministic scope. Deterministic trust paths use
explicit diffs because the git range is the content being verified.
`loom gate review` may accept `--bead <id>` only as intent/context
metadata paired with `--diff <range>`; `review --bead <id>` without
`--diff` is invalid.

`--spec` is not a gate filter. Work-surface affectedness decides which
spec annotations run, and spec-specific target discovery uses
`loom spec <label> --targets` instead. Automated trust paths therefore
cannot narrow away sibling-spec obligations by passing a spec label.

A `--diff <range>` that git itself rejects — an unknown commit, or
`@{u}` when the branch has no upstream — is a **hard error**: the gate
exits non-zero naming the range, rather than degrading to an empty
input set. A trust-bearing diff run resolves the range to concrete
base/head commits and records those commits in its `GateRun`. A valid
range that matches no files is a legitimate empty scope; a shorthand
like `--diff HEAD` remains a diagnostic working-tree-vs-HEAD mode but
is never marker-eligible.

### Deterministic verify lanes

`loom gate verify` has one deterministic contract across all callers:

| Invocation | Project hook lane | Spec annotation lane |
|---|---|---|
| `loom gate verify --files <paths...>` | none | Affected `[check]` and `[test]`; `[system]` excluded |
| `loom gate verify --diff <base>..<head>` | `prek run --hook-stage pre-commit --from-ref <base> --to-ref <head>` | Affected `[check]` and `[test]`; `[system]` excluded |
| `loom gate verify --tree` | none | Full-tree `[check]`, `[test]`, and `[system]` |
| `loom gate verify --target <target>` | none | Exact matched target in its tier; no scope filtering |

The project hook lane is first-class trust, not hidden magic: project
policy lives in `.pre-commit-config.yaml`, and prek decides hook file
selection, `always_run`, and filename passing. Loom does not reinterpret
project hook policy. The project hook lane runs before spec annotations;
if a hook modifies the working tree, `verify` exits non-zero with a
tree-modified-by-hook failure so the caller stages/commits or rolls
back before retrying. Successful `verify` requires the tree to remain
unchanged after hooks.

Diff-scope recursion is guarded by the parent gate run, not by a
project-specific hook id. When a parent `verify --diff` invokes prek
and the project's pre-commit stage includes `loom gate verify --files`,
the nested files invocation records a skipped gate event with reason
`parent-diff-gate` and exits 0 because the parent run will execute the
annotation lane after prek returns. The canonical hook id may be used as
an optimization, but correctness cannot depend on it.

`[system]` does not run by default under finite `verify --diff` or
`verify --files` scopes. It runs under `verify --tree`, explicit
`loom gate system --diff`, CI/standing sweeps, or explicit project
pre-push hooks. Expensive `[check]` and `[test]` verifiers that should
skip under finite scopes must implement an input-query contract; unknown
inputs for eligible tiers run conservatively.

The composition: `loom gate audit` ≡ `loom gate verify && loom gate
review` for the same explicit `--diff` or `--tree` scope. Both are
inspection paths; `audit` produces no bd writes. The act path is
`loom gate mint`, which walks and writes; see [*Findings and
Minting*](#findings-and-minting).

## Stages

Same gate, four review points plus one stabilization act. Scope and
cost-of-failure differ; the underlying trust surfaces are explicit.

| Stage | Where | Scope | Cost-of-failure | Primary catches |
|---|---|---|---|---|
| **Plan** | `loom plan [SPEC_LABEL ...]` | Anchored specs plus siblings touched during interview | Lowest — no code yet | Missing claims, weak claims, missing verifier surfaces, invariant clashes in proposed spec changes |
| **Worker self-check** | In the bead container before `LOOM_COMPLETE`: `loom gate verify --diff <bead-base>..HEAD` plus prompt-level self-review | The bead branch's committed work against its injected base range | Low — the agent is still in-session | Formatting/hook failures, affected `[check]`/`[test]` failures, obvious criteria/style misses the agent can fix before final marker |
| **Per-bead integration** | In `.loom/integration` after rebase/ff: `loom gate verify --diff <pre-integration-head>..HEAD` | The exact commits just integrated into the loom workspace | Medium — one bead's worth | Cross-bead deterministic breakage after integration, project pre-commit failures, affected `[check]`/`[test]` failures. On failure, integration rolls back and the same bead retries with the gate log in `previous_failure` |
| **Stabilization** | `loom gate mint -m <molecule-id>` after no original non-deferred work remains ready | The molecule's `loom:deferred` remediation beads | Medium — amortized over a molecule, not one tiny finding | Promotes deferred remediation batches so the loop drains them before push; coalesces repeated finding hashes instead of reminting tiny beads |
| **Push** | Fetch/rebase to `origin/<integration-branch>`, run the actual prek pre-push chain for `origin/<integration-branch>..HEAD`, then `loom gate review --diff origin/<integration-branch>..HEAD` | The actual push range, not merely the molecule's original base range | Highest — **blocks push**. `GateSuccess` is constructible only from matching `VerifiedScope`, `ReviewedScope`, pre-push hook coverage, marker evidence, and clean tree/config/range fingerprints | Conformance gaps in the pushed range, project pre-push failures, integrity-gate findings in affected annotations, review concerns, dispatch errors, origin-advanced races |
| **Standing safety net** | `loom gate audit --tree` for inspection; `loom gate mint --tree` to act (on-demand, nightly CI, scheduled) | Entire spec tree × entire implementation | Catches **verifier-input under-reporting** — any verifier a finite scope would have skipped because its derived input set was too narrow is surfaced here | Cross-file incoherence finite diffs did not surface, contracts orphaned across PRs, accumulated style/test regressions, template-vs-spec drift (Invariant 3), surface drift, verifier-reported input sets that are too narrow |

The plan stage has no separate command invocation — the agent runs
the rubric inline during the planning interview, and `loom plan` is
the surface that opens that interview. Worker self-check is prompt-level
feedback; it is not marker-eligible and cannot authorize a push. The
driver's per-bead integration stage is deterministic only. Focused
per-bead LLM review is not part of the default hot path; the worker's
self-review happens inside the implementation session, and the
authoritative LLM review runs at molecule completion over the actual
push range.

The push stage is **non-optional and load-bearing across every
execution mode of `loom loop`** — default active work epic, explicit
bead/epic roots, and parallel dispatch. It synchronizes with origin before verification, rebases
local integration commits when origin advanced, verifies the exact range
that would be pushed, reviews that same range, mints a marker only for
that range, and pushes inside the same critical section. A non-fast-
forward or origin-advanced race invalidates the prior gate result; the
driver fetches/rebases and reruns the gate instead of reusing stale
evidence.

The verdict is encoded in [`GateOutcome`](harness.md#loop-outcome-types):
`Success` only when the typed gate evidence matches the final
pushable state; `Fail` on any failure with the reason explicit;
`NoGate` only for legitimate "no work to gate" terminals
(`NoBeadsReady`, `SelectionPartial`). The `GateSuccess` struct is sealed, so
a clean `loom loop` exit without the gate actually firing is
unrepresentable. The standing safety net is scheduled, not
load-bearing for any individual push — its job is to catch verifier-
input under-reporting over time, not to replace the push gate.

### Plan-stage checks

The plan stage is first-class: errors caught before code exists
are cheapest. The stage runs inside the planning interview — the
agent's rubric. Three checks must satisfy before the interview can
commit:

1. **Completeness check.** Every requirement the user expressed has a
   checkable surface: a Success Criteria bullet with a `[check]`,
   `[test]`, `[system]`, or `[judge]` annotation, a lifecycle /
   decision / contract table row, or an explicit `## Out of Scope`
   declaration. Implicit assumptions are surfaced; the agent either
   makes them testable or marks them non-testable with a reason.
   Annotations whose targets will not resolve at commit time —
   typically newly-authored claims whose verifier implementation
   lands in a follow-on `loom loop` bead — carry the pending modifier
   `?` (see [*Pending modifier*](#pending-modifier)). Applying the
   marker is part of completeness: an unmarked annotation pointing
   at a not-yet-existing target reads as a broken claim, where a
   `?`-marked annotation reads as an honest declaration of the
   surface plus an explicit acknowledgement that the implementation
   is on the way.
2. **Internal coherence check.** The spec under interview is scanned
   for internal contradiction — two sections saying different things,
   decision-table rows that conflict, prose claims that can't both be
   true.
3. **Invariant-clash scan.** Check the anchor and any touched sibling
   specs for invariants the proposed change may contradict
   (architectural / data-structure / explicit-constraint /
   non-functional / out-of-scope). On detection, pause; resolve via
   three paths.

The agent doesn't separately *run* the gate at this stage — the gate
IS the agent's rubric. A check failing means the interview stays open
until the user resolves it.

(General agent discipline: at any stage, if the agent notices the
template it's running under contradicts the spec, it raises the
contradiction as a user question. This isn't a structured rubric item
at the plan stage — it's expected awareness. Mechanical detection of
template-vs-spec drift happens at the standing safety net instead.)

### Worker and per-bead integration checks

**Agent self-check before marker emit.** In `loom loop`'s bead
container, the worker runs the injected exact self-check range before
emitting `LOOM_COMPLETE`: `loom gate verify --diff <bead-base>..HEAD`
where `<bead-base>` is the resolved base commit the driver supplied
for the bead workspace. `@{u}..HEAD` is acceptable only when the
upstream is the intended injected base; `--diff HEAD` is a diagnostic
working-tree check and is not the worker completion contract. If the
agent makes another commit or a hook changes the tree after the
self-check, the prompt requires the agent to rerun the self-check
before final marker emit.

The self-check runs the same deterministic diff contract every caller
gets: project pre-commit hooks via prek, then affected `[check]` and
`[test]` annotations. This is fast feedback for the agent, not
authoritative gate evidence. The worker also performs prompt-level
self-review before completion: re-read the bead criteria, inspect the
diff, check style/spec fit, and fix issues or emit the appropriate
worker self-report marker.

**Driver per-bead integration.** After the bead branch rebases and
fast-forwards into `.loom/integration`, the driver runs deterministic
verification only:

```bash
loom gate verify --diff <pre-integration-head>..HEAD
```

This post-integration verify runs in the loom workspace against the
actual integrated tree. It does not pass `--bead`, `--spec`, or a
hidden tier override, and it does not run a focused LLM review by
default. Any failure rolls the integration branch back to
`<pre-integration-head>`, records the gate log path in
`previous_failure`, and retries or reopens the same bead through the
existing recovery policy. The worker's `bd close` is provisional until
this deterministic integration gate passes.

**Review findings.** Authoritative LLM review runs at molecule
completion over the actual push range. Review findings carry an
explicit `route` field in addition to the concern token (see *Emit
shape*):

| Route | Meaning | Driver action |
|---|---|---|
| `blocking` | The pushed range is not acceptable: acceptance criteria unsatisfied, touched code violates style/test-quality rules, verifier added or changed by the molecule is dishonest/too narrow, or deterministic verify regressed. | Refuse the push and create or reuse same-molecule remediation work; no marker is minted. |
| `deferred` | The finding is real but broader than the pushed range's immediate acceptance surface: adjacent or pre-existing drift, cross-spec / standing-safety-net issue, or cleanup outside the touched surface. | Merge the finding into the molecule's `loom:deferred` remediation bead for the lead spec; do not make it ready until stabilization. |
| `clarify` | Human intent is required, typically an invariant clash with multiple valid resolution paths. | Create or update one `loom:clarify` bead for the finding hash, preserving the required `## Options — …` block. |

The **Concern token** still names the issue class (`spec-coherence-fail`,
`style-rule-violation`, `verifier-bypass`, `invariant-clash`, …) and
determines the target variant. The route determines workflow
behaviour. Token alone is too coarse: for example,
`spec-coherence-fail` is blocking when it invalidates the pushed work's
own criterion, but deferred when it identifies adjacent drift outside
the pushed surface.

The terminal `LOOM_CONCERN` marker is emitted at end-of-walk if any
findings were emitted (per [harness.md § Verdict Gate](harness.md#verdict-gate));
it carries no per-finding payload of its own. The terminal marker's
payload is a JSON object with a single `summary` field; routing is
per-finding via each streamed finding's `route`, never via the
terminal marker. A clarify-route finding whose evidence lacks a
well-formed options block falls back to `loom:blocked` with cause
`clarify-without-options` rather than a stranded clarify. Push is held
until clarify beads in the molecule are resolved via `loom inbox`.

### Standing-safety-net checks

`loom gate verify --tree`, `loom gate review --tree`, and `loom gate
mint --tree` form the standing-safety-net triad. The first two are
inspection-only and run independently (or compose via `loom gate
audit --tree`); the third is the act surface that produces remediation
beads. Mechanical-only inspection is fast and frequent; the full
sweep + mint is rarer.

`loom gate verify --tree` exercises every audit at tree scope: every
`[check]` / `[test]` / `[system]` verifier, all linters, all
`[check]`-tier walks the consumer has registered, walking every spec
and every implementation file.

`loom gate review --tree` runs the LLM rubric against the whole spec
set × implementation. The finite-diff rubric checks apply, scoped to
the tree rather than a diff. Additional safety-net-only checks:

- **Template-vs-spec drift** (Invariant 3 enforcement). Reads every
  template loom uses (embedded in the loom binary, plus any
  consumer-provided overrides) against every spec in the consumer's
  spec tree. Flags any template instruction that contradicts a spec
  claim. Hard fail conceptually, but surfaced as a `bd` issue (no
  "merge to refuse" at the standing safety net). Concern token:
  `template-spec-drift`; the rubric body lives in the review prompt's
  *Template-vs-Spec Drift Walk* partial, gated on `--tree` scope.

- **Cross-spec clash.** Two sibling specs in the workspace make
  incompatible claims about a shared surface — e.g. one spec defines a
  contract one way, a sibling references it differently; one spec's
  command-set table conflicts with another's prose. Concern token:
  `cross-spec-clash`. Target is `Criterion { spec, anchor }` naming
  the side the reviewer considers primary; `bonds` lists every spec
  the clash spans, and the other side(s) appear verbatim in
  `evidence` prose. Gated on `--tree` scope because finite-diff scope
  cannot see sibling-spec context.

- **Spec-conventions violation.** A spec violates a rule from
  `docs/spec-conventions.md` that the structural integrity gate
  cannot detect deterministically — un-promoted tentative annotations
  (annotations carrying placeholder language the spec author intended
  to revise before commit), a testable claim authored without any
  verifier annotation, prose that should be a success-criteria bullet
  but lives as flat prose, or any other convention violation that
  requires reading the spec's intent rather than walking a structural
  check. Concern token: `spec-conventions-violation`. Target is
  `Criterion { spec, anchor }` naming the offending location.
  (Mechanical violations — multi-annotation criteria, unresolved
  annotations, stub-pointing — are caught by the integrity gate at
  every scope, not here.) Gated on `--tree` scope.

`loom gate mint --tree` walks both the deterministic verifiers
(`verify` side) and the LLM rubric (`review` side), then mints typed
findings from both into remediation beads. The walk semantics are
identical to `verify --tree` + `review --tree` running together; the
act is what `mint` adds. Standing-stage findings route through the
spec/work-epic lifecycle in
[harness.md](harness.md#spec-and-work-epic-lifecycle):

- **Spec epics are metadata only** — every bonded indexed spec must have
  exactly one `loom:spec spec:<label>` epic. A missing spec epic is
  created, then immediately closed, as metadata; duplicate spec epics
  are a structural invariant violation and `mint` refuses before
  creating remediation work. Work beads are never parented to spec
  epics.
- **Actionable tree findings share one work epic** — after suppression,
  dedup, and structural validation, all remaining tree-scope fix-up,
  blocked-clarify, and clarify beads are parented under one standing
  remediation work epic for the mint run, regardless of lead spec. The
  driver labels that epic as the sole `loom:active` work epic so bare
  `loom loop` runs it.
- **No actionable findings means no new epic** — if the tree sweep finds
  no unsuppressed actionable findings, or every finding dedups to
  existing live work, `mint --tree` creates no work epic and leaves the
  current `loom:active` bookmark unchanged.

This is the safety property — findings about a spec with no active work
get one loopable work container, not silently dropped or scattered
across spec-local epics, while spec-local cursor metadata remains on
spec epics.

See [*Findings and Minting*](#findings-and-minting) for the full
deferred remediation processing flow, dedup mechanism, and emit shape.

This behaviour is uniform for `mint --tree` across the workspace:
`mint` has no `--spec` filter; lead-spec selection comes from each
finding's typed `bonds` and target for grouping/labels, not from CLI
narrowing or parent-epic selection.

Invariant clashes surfaced at the standing safety net raise
`loom:clarify` under the same standing remediation work epic.

### Surface-conformance audit

A deterministic audit (no LLM call) that diffs the consumer's
spec-declared user-facing surface against the compiled binary.
Closes the class of failure where the spec mandates a command or
flag the binary never grew (or fails to remove one the spec marked
removed). Implemented as a `[check]`-tier verifier rather than a
separate subcommand: the consumer annotates the relevant spec
criterion with `[check](<command that diffs declared surface against
the binary>)`. See [harness.md FR13](harness.md#functional)
for the four hard-fail dimensions and audit triggers.

**Boundary with `loom gate review`'s style-rule walk.** Help-text
wording is **not** a surface-audit dimension. CLI-style requirements
(e.g. a short single-sentence help line, no implementation
references) live under the LLM-judged style-rule walk so spec prose
can be polished without churning a deterministic gate. The surface
audit checks that commands and flags exist with the right names and
grouping — nothing about how they describe themselves.

## Findings and Minting

`loom gate mint` is the gate's sole driver-side mint surface — the
one command that walks the rubric and produces remediation beads. Every
other gate subcommand is inspection-only (no bd writes). Mint is
what makes the rubric's concerns actionable.

### Canonical contract location

The Rust contract for the gate's wire format is owned by
`loom-protocol::gate` — a leaf crate carrying the `Finding` record
struct + `ConcernToken` closed enum + `FindingTarget`
internally-tagged-on-`kind` enum + `TargetKind` + `FindingValidator`
trait + `FindingParseError` + `BadWalk` + `TerminalSurface` +
`WalkOutput` + `WalkOutputError` + `ExitSignal` + the
`parse_walk_output` / `WalkOutput::from_stdout` / `parse_exit_signal`
parsers + the `LOOM_FINDING_PREFIX` constant.

**Crate scope.** `loom-protocol` is single-purpose: cross-crate wire
protocols loom emits or consumes. Today it carries one module,
`gate`, covering the findings/concern surface this spec defines.
Future protocols (agent stream-json, pi-mono RPC, loop-phase exit
markers) may land as sibling modules (`loom-protocol::agent`,
`loom-protocol::run`, …) without re-litigating crate-extraction
overhead. Each protocol's wire-format major-bump SemVer is
governed at the module boundary by the same anti-drift pattern
`gate` uses (a single-source-of-truth partial paired with a
`[check]`-tier verifier that refuses restatement elsewhere; see
*Emit shape* below).

**Dependency direction.** Leaf crate. Depends on `serde` + `serde_json`
(JSON wire), `thiserror` / `displaydoc` (error types), `blake3` (the
finding-hash crate; see *Finding id, finding hash, suppression,
and dedup* — algorithm is implementer's choice, but the dep set is
closed), and `loom-events`
for `SpecLabel`. No Askama, no bd client, no template prose — those
live one layer up. `loom-templates`, `loom-workflow`,
`loom-gate`, and the loom CLI all depend on `loom-protocol`;
`loom-templates` re-exports the `gate` module's public types via
`pub use` so existing `PreviousFailure::ReviewConcern { findings }`
construction works without consumers touching the dependency graph.

**`pub` / `pub(crate)` boundary.** The public surface is the typed
contract a consumer needs to construct, match on, or read from a
parsed walk: `Finding`, `ConcernToken`, `FindingTarget`, `TargetKind`,
`FindingValidator`, `FindingParseError`, `BadWalk`, `TerminalSurface`,
`WalkOutput`, `WalkOutputError`, `ExitSignal`, `LOOM_FINDING_PREFIX`,
`parse_walk_output`, `WalkOutput::from_stdout`, `parse_exit_signal`,
and `Finding::id` / `Finding::hash`. The following stay `pub(crate)` so the
implementation can reshape without a major bump: per-layer validators
inside `Finding::validate`, per-variant `canonical_form` identity
helpers, `Finding::parse_payload` (single-line parser — consumers go
through `parse_walk_output` for the full pipeline), and internal
helpers like `terminal_surface_from_stdout`. Widening later is cheap;
narrowing is a breaking change.

**The seal is field-private, not constructor-private.** The
silent-loss failure class — production caller constructs `WalkOutput`
with bogus fields and the typed terminal/finding pipeline is bypassed
— is structurally unrepresentable because `WalkOutput`'s fields are
private at the `loom-protocol` crate boundary.
`WalkOutput::from_stdout` is `pub` so consumers can call it, and
it's the only construction path. `Finding`'s fields stay `pub` for
read access — consumers match on `token`, `bonds`, `target`,
`evidence` after parsing — and any `Finding` reaching mint came
from `parse_walk_output`, so the validator's guarantees ride
through with it.

**SemVer = wire format stability.** The crate's MAJOR version is the
protocol version. A breaking wire change (renamed token, retyped
target shape, removed enum variant) requires a major bump; consumers
opt in via Cargo. Additive changes (new `ConcernToken` variant, new
`FindingTarget` variant, new fields with `#[serde(default)]`) are
minor bumps. No `"protocol": <n>` field appears on the wire — the
existing typed `FindingParseError::Json` (which carries the serde
unknown-variant error verbatim) and `FindingParseError::TokenVariantMismatch`
give loud, structural breakage when a consumer's protocol-crate
version doesn't match the loom binary it spawns. Cargo + Cargo.lock
pinning coordinates the two halves of the pipeline; no per-line
versioning needed.

**Cross-repo consumers.** External consumers (e.g. wrix) depend on
`loom-protocol` directly. The expected consumption shape is:
spawn `loom gate review` / `loom gate mint` as a subprocess, capture
stdout, call `loom-protocol::gate::parse_walk_output(&stdout,
&validator)`. The typed `WalkOutput` is the consumer's entry point
into the parsed walk. Compile-time type safety + the leaf-crate
dependency shape gives consumers the same guarantees loom's own
internal pipeline has.

The contract types previously defined at `loom-templates::finding`
relocate to `loom-protocol::gate` in a single atomic migration
diff. The `finding_no_duplicate_definitions` walker continues to
enforce the single-definition property across the workspace:
[check](cargo run -p loom-walk -- finding_no_duplicate_definitions)

The wire format's sole textual definition for *agent-facing prose*
lives at `crates/loom-templates/templates/partial/findings_walk.md`;
the anti-drift `[check]`-tier verifier (defined in *Emit shape →
Single source of truth* below) refuses any template that restates
the `LOOM_FINDING:` / `LOOM_CONCERN:` colon-suffixed forms outside
that partial. The partial documents the wire format for LLM agents;
`loom-protocol` documents it for Rust consumers. They are pinned to
the same loom release via Cargo + the workspace's git ref; the
existing anti-drift walker covers both surfaces.

**`ConcernToken` is not `ReviewConcern`.** Two enums look similar
and live in different crates with different purposes. `ConcernToken`
(in
`loom-protocol::gate`) is the **wire-level identifier** on each
streamed `LOOM_FINDING:` record — the closed set of tokens
(`spec-coherence-fail`, `orphan-integration`, `verifier-bypass`,
…) the rubric emits and `loom gate mint` routes on. `ReviewConcern`
(in `loom-workflow::review::phase_verdict`) is a separate 12-variant
enum that previously named the terminal `LOOM_CONCERN` token; under
the retired terminal-token contract (per the review rubric's
*Streaming + terminator pairing rule*), the terminal carries only
`{"summary": "..."}` and per-finding routing is decided on each
`LOOM_FINDING:` record's `ConcernToken`, not on the terminal.
`ReviewConcern` survives as a **display vocabulary** for
`bd update --notes` and verdict-log human-readable cause labels
(derived from `findings[0].token` or a "multiple" label when
heterogeneous); it has no routing role.

### Inspection vs. act partition

Every gate subcommand except `loom gate mint` is **inspection-only**
— it walks rules and emits findings to stdout but performs no `bd`
writes. `mint` is the sole bd-mutation chokepoint. The partition is
structural, not advisory: no code path inside `loom gate audit` /
`verify` / `review` / `judge` / `rubric` / `check` / `test` / `system`
/ `verify-marker` may call into the mint pipeline's `bd` write surface
as a side-effect. A `[check?]`-tier verifier asserts this (deferred
to land alongside the broad forward-resolution change under
[*Pending modifier*](#pending-modifier) below) by scanning
production sources for `mint_findings` / `mint_finding_with_options`
invocations outside `loom-workflow::mint` and outside `loom loop`'s
verdict-gate routing path.
[check](cargo run -p loom-walk -- audit_makes_no_bd_writes_outside_mint_module)

The driver's `loom loop` per-bead path is an **operator-level
composition** around the gate, not a side-effect of any inspection
subcommand: after integration it runs deterministic
`verify --diff <pre-integration-head>..HEAD` and records a typed gate
log. The molecule-completion push gate deliberately composes
pre-push deterministic verification with `review --diff <actual-push-range>`
without invoking `mint`; findings ride through the review-log file and
through the typed recovery/remediation surfaces. Stabilization or an
explicit `loom gate mint` invocation is what consumes findings as bd
state.

The `MarkerProof` mint at the molecule-completion push gate (see
`## Marker` below) is a **separate** mint surface, owned by
`loom-gate::marker` with its own `pub(crate)` constructor — it
writes a single content-addressed JSON file to
`.loom/marker.json`, never bd state. "Audit makes no bd
writes" remains true through that path; the marker is filesystem
state, not bd state.

### Wire-format mixed-shape principle

The wire format the rubric walk emits is shaped by one principle
that governs every marker the driver consumes: **JSON-payload for
markers the driver routes on, bare for markers reading adjacent
prose.** `LOOM_FINDING:` and `LOOM_CONCERN:` carry JSON because the
driver routes per-finding tokens and the terminal summary needs
structured framing.
`LOOM_COMPLETE` / `LOOM_NOOP` / `LOOM_RETRY` / `LOOM_BLOCKED` /
`LOOM_CLARIFY` are bare because the parser reads context (reason /
question) from the prior non-empty line; LLM agents narrate the
reason in prose and emit the marker as a yes/no terminator without
having to compose a JSON object in the same turn. Mixing in either direction — JSON payload
for a bare marker, bare payload for a routed marker — is a
wire-format violation and is rejected by the typed parser
(`loom-workflow::todo::exit::parse_exit_signal` for terminals;
`loom-workflow::review::finding::parse_walk_output` for the streaming
finding lines).

### Scope-dependent walk

`mint` is the act surface, so its scopes are intentionally narrower
than inspection commands:

| Scope | Walks / consumes | Why |
|---|---|---|
| `-m`, `--molecule <id>` | Consumes the molecule's already-recorded deferred finding beads and promotes them to ready remediation work. It does not run new verifiers or review. | Stabilization is molecule-local; original implementation work drains before deferred findings become ready. |
| `--tree` | Deterministic verifiers + LLM rubric over the full workspace, then creates or updates ready remediation batches under one active work epic when actionable findings remain. | Standing safety-net runs have no current push or worker session to recover into; deterministic failures have no other home. |

`mint` rejects `-b/--bead`, `--diff`, `--files`, `--target`, and
`--spec`; those scopes are inspection-only (`review` / `audit`) or
deterministic-only (`verify`). Bare `loom gate mint` prints subcommand
help and runs nothing; callers choose `--tree` or `-m/--molecule`
explicitly.

### LOOM_INSIDE guard

`loom gate mint` refuses to run when `LOOM_INSIDE=1`. Deterministic
gate inspection subcommands may run inside a bead container for
self-check and local diagnostics. `mint` remains banned
because bd writes from inside a loom-managed container would mutate
driver-owned state rather than act as a local inspection. LLM-spawning
gate subcommands follow the harness-level `LOOM_INSIDE` guard. The
check is a deterministic precondition; no walk runs and no exit code 2
path fires.

### Concern tokens and target variants

Every finding carries a typed `target` whose variant is determined
by the `token`. The driver canonicalizes the variant when computing
the finding id (under *Finding id, finding hash, suppression, and
dedup* below) so the same finding hashes the same way across rubric
runs. Rubric-origin findings also carry the explicit `route` field
from *Worker and per-bead integration checks*; the table below names
the default route when no push-range classification applies. At tree
scope, `route="deferred"` still materializes ready remediation because
`loom gate mint --tree` is an explicit standing-safety-net act; a stray
`route="blocking"` from the tree rubric is accepted as the same ready
remediation for compatibility.

| Token | Source | Target variant | Default route |
|---|---|---|---|
| `spec-coherence-fail` | Rubric (conformance trace) | `Criterion { spec, anchor }` | deferred |
| `orphan-integration` | Rubric (contract closure) | `Contract { id }` | deferred |
| `style-rule-violation` | Rubric (style-rule walk) | `StyleRule { rule_id, subject }` | deferred |
| `verifier-bypass` / `weak-assertion` / `fabricated-result` / `coincidental-pass` | Rubric (verifier-honesty walk) | `Annotation { target_string }` | deferred |
| `mock-discipline` | Rubric | `TestPath { path }` | deferred |
| `verifier-too-narrow` | Rubric | `Criterion { spec, anchor }` | deferred |
| `concurrency-untested` | Rubric | `LockSite { file, line }` | deferred |
| `judge-flag` | Rubric (`[judge]` criterion) | `Criterion { spec, anchor }` | deferred |
| `invariant-clash` | Rubric (invariant-clash scan) | `Invariant { spec, section, tag }` | **clarify** (evidence MUST embed `## Options — …`; mint falls back to blocked otherwise — see *Deferred remediation processing*) |
| `template-spec-drift` | Rubric (tree-scope only) | `Template { path }` | deferred |
| `cross-spec-clash` | Rubric (tree-scope only) | `Criterion { spec, anchor }` | deferred |
| `spec-conventions-violation` | Rubric (tree-scope only) | `Criterion { spec, anchor }` | deferred |
| `verifier-failed` | Deterministic verifier exit ≠ 0 (tree-scope only) | `Annotation { target_string }` | deferred |
| `dispatch-error` | Verifier exit 2 — command not found / missing prereq (tree-scope only) | `Annotation { target_string }` | deferred |
| `unresolved-annotation` | Integrity gate forward-resolution (tree-scope and push-gate scope) | `Annotation { target_string }` | deferred |
| `stub-pointing` | Integrity gate stub-pointing (tree-scope and push-gate scope) | `Annotation { target_string }` | deferred |
| `unneeded-pending-marker` | Integrity gate stale pending modifier (tree-scope and push-gate scope) | `Annotation { target_string }` | deferred |
| `inputs-protocol-error` | Integrity gate inputs-protocol check (tree-scope and push-gate scope) — an opted-in input-query (a `[judge]` collect mode, or a `[check]` / `[system]` runner that declares an `inputs` query) exited non-zero or emitted a malformed inputs document | `Annotation { target_string }` | deferred |
| `multiple-annotations` | Integrity gate atomic-acceptance (tree-scope only) | `Criterion { spec, anchor }` | deferred |
| `pending-marker-resolved` | Sweeping walker (any scope) — a pending element (`?` or `~`) in structured spec input has resolved against the pending direction (`?` + present, or `~` + absent), so the author must drop the marker to its resolved value | `MatrixCell { spec, partial, template }` / `SurfaceElement { spec, kind, name }` / per-walker variant | deferred |

**Clarify-route subset.** Today only `invariant-clash` defaults to
`clarify`; the rest default to `deferred`. At push-range review the
explicit `route` field may classify any non-clarify token as
`blocking` or `deferred` depending on whether it invalidates the pushed
work or identifies broader drift. Adding a future default-clarify token
is a one-row table edit + the new token's enum entry; no per-token
carve-out in the mint pipeline.

`scope-creep` and `scope-shortfall` are finite-diff review tokens; the
tree-scope walk does not emit them, and mint never receives them from a
tree-scope source.

The target variant is architecture-bearing — its shape is what
makes "every finding carries a target appropriate to its token"
structurally unrepresentable as a mismatch. See [`spec-conventions.md`
*In scope #4*](../docs/spec-conventions.md).

### Emit shape

The LLM rubric walk emits findings as streaming records on stdout
from the agent's subprocess. Each record starts with a `LOOM_FINDING:`
prefix followed by a JSON payload:

```
LOOM_FINDING: {"token":"<token>","route":"blocking|deferred|clarify","bonds":["<spec>",...],"target":<target>,"evidence":"<evidence>"}
```

- **`token`** — concern identifier from the closed-set enum in
  *Concern tokens and target variants* above.
- **`route`** — rubric-origin workflow route. `blocking` refuses the
  current push-range review and creates or reuses same-molecule
  remediation work, `deferred` merges into a `loom:deferred`
  remediation bead for molecule stabilization, and `clarify`
  materializes a human-decision bead with options. Tree-scope rubric
  output should emit `deferred` for mechanical remediation and
  `clarify` for human decisions; if it emits `blocking`, the parser
  keeps the finding and tree mint materializes it as ready remediation.
  Tree-scope deterministic findings normalized by the driver do not
  come from LLM wire output; the driver assigns `deferred` at
  molecule/push scope and materializes ready remediation directly at
  tree scope.
- **`bonds`** — array of spec labels the remediation should bond to.
  Always present, always at least one element. The driver picks the
  bonding lead from this array via *Multi-spec findings* below.
- **`target`** — tagged JSON object whose `kind` discriminator
  selects the variant per the table above; carries
  identity-bearing fields specific to the variant.
- **`evidence`** — the rubric's reasoning, stored verbatim on the
  remediation bead's description or verdict/recovery context. For
  `route="clarify"`, evidence MUST embed the canonical
  `## Options — …` block per the *Options Format Contract*. Gate
  routing validates this at parse time and falls back to
  `loom:blocked` with cause `clarify-without-options` when the
  options block is absent — see *Deferred remediation processing* below.

`bonds` is *bonding* metadata; `target` is *identity* metadata. The
two are kept separate so the driver can shift bonding (e.g., as
molecules open/close over time) without invalidating the
finding's id and hash.

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

Emit compact one-line JSON where possible, as the finding is
identified (not batched at end-of-walk). Long evidence and
`route="clarify"` Options blocks are allowed to span lines inside JSON
string values: before `serde_json` deserialization, the driver
normalizes raw line breaks that appear inside a JSON string to `\n`
escapes, then applies the same typed validation. Newlines outside
strings are accepted only as ordinary JSON whitespace within the same
object. JSON was chosen over pipe-delimited
specifically because LLM emit is more reliable on JSON than on bespoke
formats — the target's tagged-union shape encodes naturally, escaping
is well-known, and field-order independence avoids one class of
malformed emit.

**Strict parse-time validation.** The `LOOM_FINDING:` prefix is
matched by substring search in the agent's stdout, so backtick-wrapped,
markdown-fenced, or prose-prefixed records are still detected. The
match is case-sensitive on the literal string `LOOM_FINDING:` (with the
trailing colon); bare-prose mentions without the colon (e.g. *"the
`LOOM_FINDING` marker"*) do not match by design.

A record that matches the substring but fails the strict validation
that follows — malformed JSON after raw string line-break
normalization (most common: trailing backticks from markdown fencing),
unknown `token`, any element of `bonds` that doesn't resolve to a
workspace spec label, `target` variant mismatching `token`'s expected
variant, or unresolved target content (criterion anchor not in spec,
file path absent on disk) — surfaces as
`BadWalk::MalformedFinding { errors, terminal }` per the pairing-rule
table below, with the well-formed terminal preserved alongside the
per-record errors. **No silent skip.** The substring-then-strict-
validate shape catches accidentally-fenced finding emit while loudly
typing the malformation, which is what makes the wire format observable
rather than fragile.

The walk is assumed to be retry-friendly: a re-run typically gets
the shape right; a persistently-malformed emit is signal that the
prompt or rubric needs adjusting.

Deterministic verifiers do **not** emit `LOOM_FINDING:` records — they
continue to follow the existing *Verifier-runner contract* (JSON verdict
on stdout, exit codes). In push and tree act contexts, the driver
normalizes each failed verifier verdict into the same typed Finding
record the LLM rubric's lines parse into, then applies the same routing
flow uniformly. The mapping:

| Verifier outcome | `token` | `bonds` | `target` | `evidence` |
|---|---|---|---|---|
| `[check]` / `[test]` / `[system]` exit ≠ 0 (and ≠ 2, ≠ 77) | `verifier-failed` | `[<spec owning the annotation>]` | `Annotation { target_string }` | verifier's JSON `evidence` field, else stderr tail |
| Exit code 2 (dispatch error) | `dispatch-error` | same | `Annotation { target_string }` | command-not-found / missing-prereq message |
| Integrity gate: forward-resolution failure | `unresolved-annotation` | `[<spec owning the annotation>]` | `Annotation { target_string }` | "annotation does not resolve" with spec:line |
| Integrity gate: stub-pointing | `stub-pointing` | same | `Annotation { target_string }` | "annotation points at stub function" |
| Integrity gate: atomic-acceptance violation | `multiple-annotations` | same | `Criterion { spec, anchor }` | "criterion carries N annotations, expected 1" |
| Integrity gate: stale pending modifier | `unneeded-pending-marker` | same | `Annotation { target_string }` | "annotation is now resolved — drop the ? marker" with spec:line |
| Integrity gate: inputs-protocol error | `inputs-protocol-error` | `[<spec owning the annotation>]` | `Annotation { target_string }` | "opted-in input-query errored / emitted a malformed inputs document" with spec:line |

The owning spec for `bonds` is the spec containing the annotation
the verifier was dispatched for — the same spec-section auto-include
the verifier's input set already uses (per *Verifier inputs*). Exit
code 77 is a skip, not a failure; it produces no Finding. The
`LOOM_FINDING:` wire format is the LLM rubric's emit shape; the
typed Finding record is the in-driver representation both sources
converge on.

The walk terminates with exactly one terminator on the final
non-empty line (per [harness.md § Verdict
Gate](harness.md#verdict-gate)): `LOOM_COMPLETE`, `LOOM_CONCERN`,
`LOOM_RETRY`, or `LOOM_BLOCKED`. `LOOM_RETRY` indicates the walk could
not complete for environmental reasons (logs corrupt, workspace
inaccessible, transient IO) and a fresh dispatch should retry the walk —
preferred over `LOOM_BLOCKED` for the "I couldn't review" failure mode
unless the reviewer also has no candidate resolution to enumerate.
`LOOM_BLOCKED` means the walk could not complete, the reviewer has no
candidate resolution to surface, and the reason explains why options
cannot be safely enumerated. Direct `LOOM_CLARIFY` is not a review
terminator: a reviewer that can enumerate options emits a
`route="clarify"` finding with the Options block in `evidence` and
terminates with `LOOM_CONCERN`. `LOOM_COMPLETE` and `LOOM_CONCERN` are
the verdict-carrying terminators and are governed by the pairing rule
below.

**`LOOM_CONCERN` payload — JSON shape and parse discipline.** The
payload is a JSON object with a single required field, `summary`,
whose value is a non-empty string:
`LOOM_CONCERN: {"summary": "<one-sentence summary>"}`. The driver
parses the payload with the same `serde_json` pipeline that
consumes `LOOM_FINDING:` records. Parse failures — invalid JSON,
missing `summary`, empty `summary` — surface as the typed
`BadWalk::Concern { payload }` recovery cause (defined in
[harness.md](harness.md#verdict-gate)) so the recovery prompt can
carry the literal text that failed and the agent can fix the
shape on the next iteration. The summary is for the verdict log
only; the actionable detail lives in the streamed `LOOM_FINDING:`
records, and per-finding routing is decided by `loom gate mint` on
each finding's token, not on the terminal marker. The terminal
token-and-reason form (`<token> -- <reason>`) is retired; the
terminal token only ever duplicated the strongest finding's token
at the cost of structural complexity.

**Streaming + terminator pairing rule.** The walk is a streaming
process: `LOOM_FINDING:` records are emitted as concerns are
identified; the terminator is the final line. The driver first
cross-checks the raw stream against the terminator for wire-shape
honesty; suppression is applied only after the shape is well-formed.
If the terminator and raw stream disagree, the run fails with a typed
`BadWalk` recovery cause:

| Finding stream | Terminator | Verdict |
|---|---|---|
| 0 | `LOOM_COMPLETE` | clean — phase done |
| ≥1 well-formed | `LOOM_CONCERN: {"summary":"..."}` | Apply rubric suppressions to the parsed findings. If ≥1 unsuppressed finding remains: recovery — `RecoveryCause::ReviewConcern { summary, findings: Vec<Finding> }` threaded into `previous_failure` (mint consumes separately). If every parsed finding is suppressed: clean — status output records the suppressed findings and the phase completes. |
| 0 | `LOOM_CONCERN: {"summary":"..."}` | `BadWalk::ConcernWithoutFindings { summary }` — concern claimed without enumeration |
| ≥1 well-formed | `LOOM_COMPLETE` | `BadWalk::FindingsWithoutConcern { finding_count, findings: Vec<Finding> }` — findings streamed but terminator claims clean; the parsed findings ride through so the next iteration's prompt can name them |
| ≥1 record failed parse | any | `BadWalk::MalformedFinding { errors: Vec<FindingParseError>, terminal: TerminalSurface }` — per-record errors are preserved alongside the typed terminal surface (well-formed terminal kept as-is; when the terminator also fails parse, the terminal is carried via `TerminalSurface::Malformed { payload }` so both failure pieces ride through the `MalformedFinding` variant) |
| any well-formed (only) | `LOOM_CONCERN:` with malformed JSON / missing / empty `summary` | `BadWalk::Concern { payload, parsed_findings: Vec<Finding> }` — payload parse failure carries the literal malformed text AND any well-formed findings that streamed ahead of the bad terminator |
| any | missing or duplicate marker | `SwallowedMarker` (existing) |

**Maximum-context preservation invariant.** Every `BadWalk` variant
carries the maximum well-formed context by struct shape. Failure
mode "lost the agent's diagnosis when one piece of the walk was
malformed" is structurally unrepresentable — the type cannot be
constructed without the parseable pieces (well-formed findings
preserved alongside a malformed terminal; well-formed terminal
preserved alongside malformed findings). This is what
`templates.md`'s `PreviousFailure::BadWalk(BadWalk)` rendering
relies on to produce a useful recovery prompt regardless of which
piece failed parsing.

**Agent's mental model.** Review the diff. Every time you identify
a concern, immediately emit a `LOOM_FINDING:` record with the
structured JSON detail and continue reviewing. When the walk is
complete, end your response with `LOOM_COMPLETE` if you found
nothing, or `LOOM_CONCERN: {"summary": "<one-sentence summary>"}` if you
emitted one or more `LOOM_FINDING:` records. The terminator must
match the stream: `LOOM_COMPLETE` means zero findings,
`LOOM_CONCERN` means ≥1 finding.

**Single source of truth.** The wire-format definitions for both
`LOOM_FINDING:` and `LOOM_CONCERN:` live exactly once, in
`crates/loom-templates/templates/partial/findings_walk.md`. Other
templates that need to talk about these markers `{% include %}`
that partial; they never restate the format. The bare-marker
partials (`partial/progress_markers.md` for `LOOM_COMPLETE` /
`LOOM_NOOP`, `partial/self_report_markers.md` for loop/todo
`LOOM_RETRY` / `LOOM_BLOCKED` / `LOOM_CLARIFY`, and
`partial/review_self_report_markers.md` for review cannot-complete
self-reports) describe bare-marker semantics without redefining the
review-walk markers.

A `[check]`-tier verifier enforces this mechanically: it scans
every file under `crates/loom-templates/templates/` for the literal
substrings `LOOM_CONCERN:` and `LOOM_FINDING:` (the colon-suffixed,
wire-format forms — bare-prose mentions like *"the `LOOM_CONCERN`
marker"* are unaffected) and fails if they appear in any file other
than `partial/findings_walk.md`. Templates that violate this fail
`loom gate check` via the [`check`]-tier dispatcher's non-zero
exit code.

### Structural enforcement

The review-phase classifier signature (`classify_review_phase` in
`loom-workflow::review::production`) consumes a typed `WalkOutput`
product (`{ terminal: TerminalSurface, findings: Vec<Finding>,
finding_errors: Vec<FindingParseError> }`), not raw `&str`.
`WalkOutput::from_stdout` is the only construction path: it takes the
agent's combined stdout and a `FindingValidator`, runs the parse
pipeline once, and returns the typed product. The classifier cannot
be called with raw `&str`; that becomes a compile error. The
silent-loss failure class — a production caller constructs
`GateInputs` without invoking the walk parser, leaving the typed
finding stream at default empty so every well-formed `LOOM_CONCERN`
with streamed findings collapses to `BadWalk::ConcernWithoutFindings`
— becomes structurally unrepresentable.

The seal is **field-private**: `WalkOutput`'s fields are private at
the crate boundary, and `WalkOutput::from_stdout` is `pub`
(consumers depending on `loom-protocol` need to call it). Field
privacy is what makes the silent-loss class unrepresentable —
struct-literal construction with bogus fields cannot compile, so
any `WalkOutput` reaching the classifier ran the typed parse
pipeline. This mirrors the sealed-`MarkerProof` pattern
(`## Marker` below): validated construction through a single
entry point is the type-shape contract for trust handoff.

### Verification surface

The runtime contract is verified at two layers — a behavioral
matrix walking every cell of the failure surface, and a property
invariant pinning the typed Finding's round-trip identity.

**Behavioral matrix (enumerable cells).** A parameterised test walks every cell of the
(stream-shape × terminal-shape) failure surface:

- **Stream-shape axis (4 cells):** zero `LOOM_FINDING:` records; N
  well-formed findings; N well-formed + M malformed (mixed); all-
  malformed.
- **Terminal-shape axis (6 cells):** `LOOM_COMPLETE`; `LOOM_NOOP`;
  `LOOM_CONCERN:` with valid JSON; `LOOM_CONCERN:` with the legacy
  `<token> -- <reason>` shape; `LOOM_CONCERN:` with malformed JSON
  (missing field, empty `summary`, invalid JSON); no terminal on the
  final non-empty line.

24 cells. Each cell asserts (a) the typed outcome variant, (b) the
maximum-context preservation invariant (every parseable piece of the
input appears in the outcome), (c) the
`Display for PreviousFailure` rendering is non-empty and references
both pieces when both are present.

No historical-log paste-ins; the matrix covers the general class.
The one existing one-shot replay test
(`legacy_token_reason_payload_routes_to_bad_walk_concern` from
lm-448x.4) stays as one regression test for the legacy
`<token> -- <reason>` form's `BadWalk` routing but is not the
load-bearing class coverage.

**Round-trip property invariant.** For every constructible
`Finding` (every `ConcernToken` × `FindingTarget` canonical
combination), `serde_json::to_string(&finding)` → embed in
`LOOM_FINDING:` record → embed in a synthetic walk output with an
arbitrary well-formed terminator → `parse_walk_output` → assert
byte-equal to the input `Finding` and finding id / hash identical.
Extends `loom-protocol::gate::tests::finding_identity_is_stable_across_runs`
from "identity stable" to "full struct round-trip."

### Finding id, finding hash, suppression, and dedup

Dedup identity is **per finding**, not per batch. Batches are a
presentation and work-queue convenience; changing which sibling
findings happen to appear in the same mint run must not change the
identity of any underlying issue.

For each parsed and validated finding the driver computes two values:

- **Finding id** — the canonical, human-readable, versioned semantic
  identity. This is the contract.
- **Finding hash** — a compact hash of the finding id, used for bd
  labels and queries. This is an index key, not the semantic
  contract.

Finding ids use lower-kebab-case for vocabulary Loom controls and
carry an explicit identity-version prefix:

```text
v1:criterion:verifier-too-narrow:gate#verifier-honesty
v1:invariant:gate#out-of-scope#inline-suppressions
v1:style-rule:rs-3:crates-foo-src-generated-rs
v1:annotation:verifier-bypass:<normalized-target>
```

The id is **target-centred**. Target kinds that already identify a
single concern class omit the token (`style-rule`, `invariant`,
`template`, `test-path`, `lock-site`). Broad target kinds that can
host multiple concern classes include a short lower-kebab concern
segment (`criterion:verifier-too-narrow:...`,
`annotation:verifier-bypass:...`). The canonicalizer is part of
`loom-protocol::gate`: a validated `Finding` exposes its id by
combining the `ConcernToken` with the typed `FindingTarget`'s
canonical key. The LLM never emits ids or hashes.

The id deliberately excludes volatile material: evidence text,
options prose, line numbers, batch size, sibling batch membership,
current bd parent, and `bonds` ordering. A multi-spec finding's id
follows the target it cites; cross-spec visibility still comes from
`spec:<X>` labels. If a future identity algorithm changes
canonicalization, it bumps the version (`v2`, ...); old labels remain
historical and new runs use the new version explicitly.

The finding hash is persisted on beads as a bd label:

```text
finding:<finding-hash>
```

The hash format is `v<identity-version>:<lowercase-hash>`. The hash
algorithm and length are implementation choices constrained only by
bd-label practicality and collision detection: if two different
finding ids produce the same finding hash in one mint run or against
live bd state, mint refuses with a structural collision instead of
merging them.

Before creating or updating remediation, the driver queries bd within
the owning molecule for every bead carrying `finding:<finding-hash>`.
The query includes live workflow states (`open`, `in_progress`,
`blocked`, `deferred`) and the `loom:blocked` / `loom:clarify` /
`loom:deferred` labels; closed matches are fetched separately as
same-molecule history.

- **Zero live results, zero closed same-molecule results** — the
  finding is untracked and may enter a new batch.
- **One live result** — update that bead in place or skip minting this
  finding; the run summary names the existing bead id.
- **More than one live result** — structural violation; refuse the run
  and surface the conflicting bead ids.
- **Closed same-molecule result** — treat the finding as already
  processed for this molecule. Record it as reobserved in the summary
  / deferred-batch evidence, but do not create a new bead
  automatically.

Closed beads outside the owning molecule are history, not suppression:
if the same finding reappears in a later molecule or tree sweep, mint
treats it as actionable current evidence. The summary may mention
matching closed beads for operator context, but outside history cannot
mask present drift.

`StyleRule` targets must include a concrete subject in addition to
the rule id. A target of only `rule_id` is too broad: suppressing or
deduping `rs-3` globally would disable the rule rather than track one
finding. The subject is the stable surface the violation applies to
(file path plus stable item/anchor when available, template path,
criterion anchor, command surface, or similar target-specific
identifier), normalized by the same lower-kebab canonicalizer used
for the finding id. A line number alone is not a stable subject.

#### Rubric suppression registry

Operators can suppress unwanted LLM-rubric noise in the workspace's
`loom.toml` using a top-level TOML array:

```toml
[[suppress]]
id = "v1:criterion:verifier-too-narrow:gate#verifier-honesty"
reason = "False positive: this verifier intentionally checks a broader seam."

[[suppress]]
hash = "v1:abc123def456"
reason = "False positive: generated template intentionally repeats this wording."
```

Exactly one of `id` or `hash` is required. `id` is the canonical
finding id and is preferred when readable; `hash` is the compact
finding hash for long command/path identities. `reason` is required
human context and is never parsed for routing.

Suppression applies only to rubric-origin findings (`LOOM_FINDING:`
records emitted by the LLM walk, including clarify-route tokens such
as `invariant-clash`). It never suppresses deterministic or integrity
findings normalized by the driver, including `verifier-failed`,
`dispatch-error`, `unresolved-annotation`, `stub-pointing`,
`unneeded-pending-marker`, `multiple-annotations`, and
`inputs-protocol-error`.

After raw stream / terminator shape validation, suppressed rubric
findings are removed from the gate verdict and from minting. `loom
gate review` / `rubric` / `audit` and `loom gate mint` still report a
suppressed-count summary listing each suppressed
finding id, hash, and token so the allowlist stays observable. If a
future rubric emits a changed finding whose id/hash differs, the
suppression no longer matches and the finding resurfaces.

Inline code-comment suppressions are out of scope: comment syntax is
language-specific, some target files have no comments, and many
rubric findings target specs, templates, commands, or seams rather
than one source line.

#### Finding status output

`LOOM_FINDING:` remains the agent-to-driver wire format. The driver
enriches parsed findings after validation and emits parseable status
JSON for operator/tool output; the LLM does not compute ids, hashes,
labels, suppression decisions, or dedup actions.

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

`action` is one of `reported`, `minted`, `skipped-live`,
`suppressed`, `stale-candidate`, `partial-stale-candidate`, or
`refused`. Inspection-only commands (`review`, `rubric`, `audit`)
use `reported` for unsuppressed findings; `mint` uses the lifecycle
actions. Human summaries may render the same data as prose, but the
JSON line is the machine-readable surface for suppression ergonomics
and tooling.

#### Stale and partially-stale reporting

At `--tree` scope, mint has the whole current finding set for the
selected spec(s), so it reports existing live remediation beads in scope
whose `finding:<hash>` labels no longer align with current
unsuppressed findings:

- A live bead whose finding labels have **no** hash in the current
  set is a stale candidate.
- A live bead whose finding labels are a **proper subset mismatch**
  (some current, some absent) is a partially-stale candidate.
- A live bead whose finding labels are all current remains the
  canonical tracker and dedups those findings.

V1 does **not** auto-close or supersede stale candidates. Stochastic
rubric runs can miss or rephrase findings, so closing bd state from a
single absence would be brittle. The report names stale / partial
candidate bead ids, current finding ids, and absent finding ids.
Operators or later explicit cleanup commands decide whether to close
or split them. The reporting pass does not run for molecule promotion
or finite inspection scopes because those scopes cannot prove a missing
finding is absent from the whole tree.

### Deferred remediation processing

Molecule-final review and tree sweep produce the same typed Finding
records, but they materialize differently depending on route and scope.
`route="blocking"` is valid at molecule-final review for work that must
block the push; at tree scope the canonical walker emits `deferred` or
`clarify`, and a stray `blocking` route is treated as ready remediation.

1. **Parse** each `LOOM_FINDING:` record into typed fields:
   `{ token, route, bonds, target, evidence }`. Per-record parse errors
   surface as `BadWalk::MalformedFinding` (see *Emit shape*) and the
   run is refused; the recovery cause carries the well-formed remainder
   so a re-run can fix the malformation.

2. **Compute ids / hashes and apply suppressions.** Each validated
   finding gets a finding id and finding hash per *Finding id, finding
   hash, suppression, and dedup* above. Rubric-origin findings whose
   id or hash appears in `[[suppress]]` are counted in the summary,
   receive `LOOM_FINDING_STATUS` action `suppressed`, and are removed
   from verdict / materialization. Suppression entries matching
   deterministic or integrity findings are ignored and reported as
   ineffective.

3. **Route per finding.** `blocking` findings at molecule-final review
   refuse the push and create or reuse same-molecule remediation work;
   at tree scope `blocking` is a compatibility alias for ready
   remediation because there is no current push to block. `deferred`
   findings merge into a molecule-local deferred bead at molecule scope,
   or ready remediation at tree scope. `clarify` findings create or
   update one human-decision bead per finding hash.

4. **Dedup before work-epic allocation.** Query bd by
   `finding:<finding-hash>` across live remediation statuses (`open`,
   `in_progress`, `blocked`, `deferred`) and live beads carrying
   `loom:clarify`. A live match is updated in place or skipped; more
   than one live match refuses the run as a structural violation. At
   molecule scope, a closed bead in the same molecule counts as a seen
   finding: if the same hash reappears, record it as reobserved on the
   molecule/deferred summary but do not mint another bead automatically.
   At tree scope, if every finding is suppressed, skipped, or deduped to
   live work, no work epic is created and `loom:active` is unchanged.

5. **Validate bonded specs and group by lead.** For each remaining
   deferred/remediation finding, validate that every bonded indexed spec
   has exactly one spec epic (creating and immediately closing a missing
   metadata-only spec epic for an indexed spec when the standing mint
   path owns creation). Pick the lead via *Multi-spec findings* below —
   the first element of `bonds`. Lead spec determines batch grouping and
   `spec:<label>` labels only; it does not select a parent work epic at
   tree scope. If a lead spec's batch grows too large, split by concern
   family (`spec-coherence`, `style`, `verifier-quality`, etc.), never
   by individual finding unless it is the only finding for that lead.

6. **Store molecule-deferred findings in bd.** Deferred beads are
   ordinary child tasks under the relevant molecule/work epic with
   `status=deferred`, label `loom:deferred`, one `spec:<X>` label per
   bonded spec, and one `finding:<hash>` label per contained finding.
   No `loom:fixup` label is used; a bead carrying `finding:<hash>`
   labels is already identifiable as gate-originated remediation work.
   bd's `deferred` status keeps the bead out of `bd ready` by default.

7. **Validate clarify coupling.** For each `route="clarify"` finding,
   scan `evidence` for the canonical `## Options — <summary>` heading
   followed by at least one `### Option <N> — <title>` subsection. If
   absent or malformed, route that finding as a single-finding blocked
   bead with cause `clarify-without-options` instead of applying
   `loom:clarify`.

8. **Promote stabilization work.** `loom gate mint -m/--molecule <id>`
   updates each deferred bead's description with the latest merged
   evidence, removes `loom:deferred`, sets `status=open`, and leaves
   `finding:<hash>` / `spec:<label>` labels intact. If no deferred
   beads exist, molecule mint is a no-op success. Promotion is a state
   transition, not creation of another bead.

9. **Tree sweep materialization.** `loom gate mint --tree` creates one
   standing remediation work epic for the run only when at least one
   actionable child batch remains after suppression/dedup. It parents
   every ready fix-up batch, blocked-clarify bead, and `loom:clarify`
   bead from that run under the same work epic, applies `loom:active` to
   that epic, and clears `loom:active` from any previous work epic. It
   does not use `loom:deferred` because the operator explicitly
   requested standing safety-net work. If the driver creates the epic but
   fails before creating any child bead, it closes or otherwise
   neutralizes the empty epic and restores the `loom:active` bookmark to
   its pre-run state before returning failure. If at least one child bead
   was created, the non-empty epic remains open/active and rerun relies
   on `finding:<hash>` dedup to retry only unfinished findings.

End-of-run summary (printed to stdout) lists blocking findings,
deferred findings merged, deferred beads promoted, ready remediation
batches created or updated, clarify findings raised, suppressed rubric
findings, stale candidates, refused structural conflicts, and transient
errors. For `--tree` runs that create an active remediation epic, the
summary names the epic id and the follow-up `loom loop` command. The
`LOOM_FINDING_STATUS:` JSON lines carry the parseable per-finding
details.

**Worker discretion on a promoted remediation batch.** The agent
dispatched against a promoted batch reads the description's enumerated
findings and decides whether to fix every finding in one diff, fix a
coherent subset and leave the remaining finding labels on the same
molecule's deferred bead, or emit `LOOM_CLARIFY` if no progress is
possible. A stabilization bead that produces new deferred findings
merges them back into the same molecule's deferred set rather than
spawning tiny child beads.

### Multi-spec findings

A finding can name more than one spec in `bonds` when the concern
spans seams (e.g., an `orphan-integration` contract spanning two
sibling specs). The `bonds` array is always present, always at
least one element; single-spec findings have a one-element array.

**Lead-spec selection rule (per finding).** The driver validates that
each spec in `bonds` has exactly one spec epic (creating a missing
metadata-only spec epic for an indexed spec when the standing mint path
owns the creation, then closing it immediately). It then picks
`bonds[0]` as the lead. This treats the rubric's ordering as
authoritative for primacy while keeping spec epics as metadata carriers
and keeping tree-scope parent selection on the single standing
remediation work epic.

**Batching follows the lead; parenting follows the scope.** A multi-spec
finding joins its lead-spec's batch (per *Deferred remediation
processing* step 5) — never duplicates across multiple specs' batches.
At molecule scope the resulting batch is parented under that molecule's
work epic. At tree scope every resulting batch is parented under the
single standing remediation work epic for the run. In both scopes the
batch bead carries one `spec:<X>` label per unique entry across the
**union of `bonds` over the batch's findings**, so a finding bonded to
{gate, harness} contributes `spec:harness` to a batch that mostly bonds
{gate} alone. Cross-spec searches surface the batch from every named
owner's perspective.

**Bonding shifts are not identity shifts.** The finding id excludes
`bonds` ordering and sibling batch membership. A finding therefore
dedups against the same `finding:<hash>` label even when a new spec
joins its `bonds` or the lead-spec selection changes between runs.
Lead-selection is only consulted for first-mint batch grouping and
spec-label assignment, not for identity.

**Validation rule.** For target variants that carry a `spec`
field (`Criterion` and `Invariant`), `target.spec` MUST appear in
that finding's `bonds` — the rubric cannot cite a criterion or
invariant in spec X while bonding only to spec Y. Validation
failure rejects the finding with a typed parse error and refuses
the mint run (per *Deferred remediation processing* step 1).

## Gate evidence and marker

Gate trust is represented as typed evidence in the normal JSONL event
stream, not as ad-hoc sidecar receipts. Every `loom gate` invocation
that runs work writes a gate log under `.loom/logs/gate/` and emits
`driver_event` records with typed `DriverKind` values. The log is the
replay/audit source; sqlite status and lookup indexes are caches over
that stream.

### GateRun lifecycle

`GateRun` is the typed record of one gate invocation, whether it
succeeds, fails, skips, or aborts. A run emits lifecycle events:

1. `gate_run_start` immediately when the invocation is accepted.
2. `gate_run_scope` after the scope is resolved to concrete files,
   commits, target matches, and config digests.
3. `gate_run_lane` for each project hook lane, spec annotation tier,
   review lane, or skipped nested invocation.
4. `gate_run_end` carrying the serialized `GateRun` summary.

A `gate_run_start` without a matching `gate_run_end` is meaningful:
the run was interrupted or the process died before it could finish.
When possible, an aborting process emits an explicit aborted end event,
but consumers cannot require it. `GateRun` contains compact pass
identity (tier/target/runner or hook id, duration, exit), full failure
and skip details, scope digests, relevant config digests, and log path;
it does not store large successful stdout bodies.

`VerifiedScope` is a sealed deterministic-success value derived from a
passing `GateRun`. `ReviewedScope` is a sealed review-success value
derived from a passing review run. `GateSuccess` is the push-
authorizing composite that can be constructed only from matching
`VerifiedScope`, `ReviewedScope`, pre-push hook coverage, and the
current workspace fingerprint. Push-eligible review consumes typed
`VerifiedScope` evidence for the same resolved content/scope; manual
diagnostic review may run without it, but cannot feed `GateSuccess` or
marker minting. The retired `--verify-exit` scalar is not a trust input.

Matching is content-based, not argv-string-based: same workspace tree,
same scope kind, same resolved base/head commits for diff scope, same
changed-file digest, no gate-narrowing filters, required lanes present,
and matching relevant config digests (`.pre-commit-config.yaml`,
`loom.toml`, and the spec annotation digest).

### Marker

`MarkerProof` is the content-addressed trust-bearing artifact the
driver-side push gate mints on `GateSuccess` and prek's pre-push hook
wrapper consumes to avoid rerunning work already proven for the exact
push. Its purpose is to make "the gate ran cleanly at this exact tree,
range, config, and hook coverage" a typed Rust value rather than an
ad-hoc filesystem stamp.

The mint authority lives in `loom-gate::marker`. The constructor
`MarkerProof::from_gate_success` is `pub(crate)`, accepts a sealed
`GateSuccess` (defined in [harness.md § Loop Outcome
Types](harness.md#loop-outcome-types)), computes the current workspace
fingerprint, and returns a `MarkerProof`. No code path outside the
marker module can mint a marker; no code path outside the gate-
invocation module can construct the `GateSuccess` that mint requires.
The bead-container agent cannot mint, regardless of what it writes to
disk or emits on stdout.

Architecture-bearing marker fields include:

- schema version;
- HEAD commit SHA (informational) and HEAD tree OID;
- porcelain-clean assertion at mint time;
- resolved push range (`from_ref`, `to_ref`);
- `.pre-commit-config.yaml` digest;
- references to the gate log / `GateRun` ids that produced the
  `VerifiedScope` and `ReviewedScope`;
- the set of pre-push hook ids / entries that passed in the verified
  pre-push stage.

The `commit_sha`, `tree_oid`, and range OIDs carry validated OID
newtypes. Malformed OIDs are rejected at deserialize time as typed
parse errors rather than reaching fingerprint comparison.

### Marker validation and hook coverage

Marker validation is two-layered:

1. **Workspace fingerprint.** The current worktree must be porcelain-
   clean, HEAD's tree OID must match the marker, the marker schema
   version must be supported, and the `.pre-commit-config.yaml` digest
   must match.
2. **Hook coverage.** The pre-push hook currently wrapped by
   `pre-push-checks` may short-circuit only when the marker's typed
   `GateSuccess` proves the same resolved push range, a successful
   pre-push `GateRun` includes that hook id/entry as passed, and
   successful `VerifiedScope` and `ReviewedScope` exist for the same
   push range.

If any element is absent or mismatched — dirty tree, different tree OID,
changed pre-commit config, different push range, missing/deleted gate
log evidence, hook not covered, failed lane, or missing review success
— the wrapper falls through and executes the underlying hook. Marker
validation is an optimization, never a trust bypass.

### File location and lifecycle

Marker lives at `.loom/marker.json` in the loom workspace — a single
file, overwritten on each mint. Atomic write uses `<path>.tmp` +
rename. The file lives in the loom workspace only; operator and bead
workspaces do not contain a trusted marker. Gate logs referenced by the
active marker are evidence; retention should preserve them while the
marker is active when possible. If evidence is missing, validation
fails and hooks run.

### Mint trigger

The driver-side molecule-completion push gate at the loom workspace
(per [harness.md § Verdict Gate](harness.md#verdict-gate)) is the sole
mint trigger. The sequence is:

1. Push gate acquires `index.lock` and fetches origin.
2. If `origin/<integration-branch>` advanced, the driver rebases local
   integration commits onto it; conflicts route through recovery and no
   marker is minted.
3. The driver resolves the actual push range
   `origin/<integration-branch>..HEAD`.
4. The deterministic push gate runs the actual prek pre-push chain for
   that range. The chain's `loom gate verify --diff <range>` hook
   emits its own `GateRun` / `VerifiedScope` evidence.
5. On deterministic success, the driver runs
   `loom gate review --diff <range>` and constructs `ReviewedScope` on
   clean review.
6. `GateSuccess` is constructed from the matching evidence; the marker
   is minted and written.
7. `git push origin <integration-branch>` runs inside the same critical
   section. A non-fast-forward invalidates the marker and forces a new
   fetch/rebase/gate attempt rather than reusing the old evidence.

Per-bead integration steps acquire the same lock for rebase + ff +
verify but release without minting. The push gate waits for any
in-flight integration to release before starting its own critical
section.

### Consumer contract

`loom gate verify-marker` is a diagnostic marker-validation
subcommand. It reads `.loom/marker.json`, checks the current workspace
fingerprint, and exits 0 on a current marker, non-zero on a missing,
stale, malformed, or unsupported marker. The diagnostic on stderr names
the specific `MarkerError` variant for human debugging but is not the
machine-readable contract.

The pre-push chain consumes markers through the repo-local
`bin/pre-push-checks` wrapper, while the Git hook shim that invokes prek
comes from `wrix.prekHooks` — see
[pre-commit.md § Marker integration](pre-commit.md#marker-integration).
The wrapper validates both fingerprint and hook coverage for the hook it
wraps, short-circuits on covered success, and execs the underlying
command on marker absence or mismatch. `loom gate verify-marker` is not
registered as a standalone prek hook; missing marker is the normal
operator-manual condition and must fall through, not abort.

### Forgery resistance and workspace boundary

The marker is forgery-resistant against tree-state forgery, stale
markers after edit, hook-coverage forgery, and agent verifier-execution
forgery. A hand-written JSON file can match the JSON shape but cannot
construct the sealed `GateSuccess` evidence it references, cannot make a
different tree/config/range pass validation, and cannot cause an
uncovered hook to short-circuit.

The marker is workspace-local — never trusted across machines or across
clones of the same repo. The driver's loom workspace, the operator's
`/workspace`, and bead workspaces are separate clones. The only writer
is the driver-side push gate in the loom workspace; operator and bead
workspaces fall through to the full pre-push hook chain. CI never reads
the marker; CI re-derives checks in its own sandbox.

## Mechanisms

How conformance / style / test-quality are evaluated:

- **Verifier path.** A passing deterministic verifier (`[check]`,
  `[test]`, or `[system]`) exercises the claim. Deterministic,
  mechanical. The gate trusts the verifier *only if* the test-quality
  dimension confirms the verifier is honest (Invariant 2).

- **Trace path.** An LLM trace through the consumer's current code
  finds the claim's implementation. Used when no verifier exists, or
  when the claim doesn't reduce to a single test (e.g., architectural
  invariants like *"loom never invokes `podman run` directly"*).

If both paths are available, both run. Failure on either → flag.

## Annotation resolution

Each criterion's annotation is resolved per its tier:

| Tier | Target shape | Dispatch |
|------|--------------|----------|
| `[check]` | `[check](target)` — a runner identifier (matched by a `[runner.check.<name>]` block in `loom.toml`) or an argv string | Runner-matched targets batch into one subprocess per runner and self-report inputs (see *Runners*); an unmatched target falls back to invoking its own process (often a walk binary the consumer ships). |
| `[test]` | `[test](path)` — language-native test path (e.g. `crate::module::test_name`, `tests/test_foo.py::test_bar`) | The gate collects all `[test]` targets in a single `loom gate test` invocation and issues **one** runner subprocess (e.g. `cargo nextest run -E 'test(p1) \| test(p2) \| ...'`). One process per invocation, full internal parallelism. |
| `[system]` | `[system](target)` — a runner identifier (matched by a `[runner.system.<name>]` block in `loom.toml`) or an argv string | One subprocess per `[system]` annotation — never batched. A runner match resolves the target's *inputs* (see *Runners*), but execution stays per-annotation: system verifiers are inherently slow and self-contained, so batching doesn't help. |
| `[judge]` | `[judge](path)` — file path or criterion id whose content is the LLM rubric | The gate collects all `[judge]` targets and issues concurrent LLM calls (API-level parallelism). |

### Command tokenisation

`[check]` and `[system]` targets are **argv strings, not shell
commands**. The dispatcher runs `shlex::split(command)` and treats
the first token as the binary and the remainder as argv — no shell
wrapper, no `/bin/sh -c`. Shell-only constructs do not survive
tokenisation and either fail at exec time or become literal argv
elements with surprising effects:

- Leading `!` (negation) — becomes `argv[0]`; exec fails with
  "No such file or directory".
- `|`, `&&`, `||`, `;` — become literal argv elements passed to
  the first binary.
- `>`, `<`, `2>&1` — same; no redirection occurs.
- `$(...)`, `` `...` `` — no command substitution; the literal
  text is passed.
- Globs (`*`, `?`, `[abc]`), `~`, brace expansion — no expansion;
  the literal text is passed.

Two idiomatic workarounds when an annotation genuinely needs
shell semantics:

1. **Shell-free rewrite (preferred).** Re-encode the assertion
   using a tool that does the equivalent in one process. The
   common case — *"file does NOT contain pattern X"* (the natural
   `! grep -q X file` form) — encodes shell-free as:
   ```
   [check](awk 'BEGIN{found=0} /X/{found=1} END{exit found}' file)
   ```
   Exits 0 iff `X` is absent. Same semantics, no shell.

2. **`bash -c "…"` wrapper.** When the assertion really needs a
   pipeline, a redirect, or compound logic, wrap the whole thing
   in a single shell invocation:
   ```
   [check](bash -c "grep -E 'X' file | wc -l | grep -qx '3'")
   ```
   The dispatcher sees `bash` as `argv[0]` and `-c "…"` as the
   remaining argv, which is well-formed.

Prefer the shell-free rewrite when one fits — fewer moving parts
and faster (no shell-startup overhead per invocation). Reach for
`bash -c` when the natural shell-encoding is materially clearer
than any single-tool equivalent.

### Pending modifier

A `?` between the tier name and the closing `]` marks an annotation
as **pending** — its target is expected not to resolve yet because
the implementation will land in a follow-on bead. Grammar:
`[tier?](target)`. The modifier is uniform across all four tiers:
`[check?](...)`, `[test?](...)`, `[system?](...)`, `[judge?](...)`.

The pending modifier exists to let `loom plan` declare the
checkable surface for a not-yet-implemented claim and commit/push
that declaration without the integrity gate refusing the push.
Without it, plan output cannot ship through its own gate; operators
face a choice between `--no-verify` bypass and hand-curated
external allowlists — neither acceptable.

Per-annotation integrity outcome:

| Modifier | Target resolves? | Outcome |
|---|---|---|
| absent | yes | silent pass |
| absent | no | `UnresolvedAnnotation` finding |
| `?` | no | silent pass (pending) |
| `?` | yes | `UnneededPendingMarker` finding — implementation landed; the `?` must be dropped |

For `[test?]`, the modifier additionally suppresses
`StubTestFunction` findings while the function body remains
`_pending_stub`; once the body becomes real evidence,
`UnneededPendingMarker` fires the same way as for plain resolution.
The two findings both express *"implementation not present yet,"*
so a single modifier suppresses both.

**Dispatch-side skip.** Pending-marked annotations are **skipped
at verifier dispatch** — `loom gate verify` / `check` / `test` /
`system` / `judge` / `mint` does not execute the verifier for a
`[tier?](target)` annotation. Only the integrity gate's
forward-resolution check runs, which is what fires
`UnneededPendingMarker` when the target newly resolves. Without
dispatch-side skip, planning sessions that author `[check?]` for
not-yet-existing walks would break their own gate verify path on
the next CI run — the verifier would execute, exit non-zero
("command not found"), and surface as a verify-fail; the `?`
discipline would be unusable in the very flow it was added to
support.

**Forward-resolution executes the command.** The integrity gate's
forward-resolution check runs the annotation's command in the same
dispatch environment as `[check]` / `[test]` / `[system]` would use
for the non-pending form, and inspects the exit code:

- Exit 0 → the assertion holds; fire `UnneededPendingMarker` (the
  `?` is stale and must be dropped in the same diff).
- Exit non-zero → the assertion does not hold; silent pass (still
  pending).

This broader check is what makes the `?` modifier honor the
author's intent uniformly across binary-pending (the verifier
executable doesn't exist yet — first-token-on-PATH fails) and
assertion-pending (the verifier exists but the asserted condition
isn't true yet — e.g. `[check?](grep -q 'pub enum BadWalk'
crates/loom-templates/src/previous_failure.rs)` where `grep`
resolves but the symbol doesn't yet appear in the file). Both
fail-modes produce non-zero exit; both are silent-pass under the
modifier. When the implementation lands and the assertion newly
holds, `UnneededPendingMarker` fires uniformly.

Two boundary conditions:

- **Command convention is read-only.** Verifier commands are
  read-only by convention (same convention that applies to
  non-pending `[check]` / `[test]` / `[system]`). The integrity
  gate executes pending-marked commands during forward-resolution,
  so a side-effectful command would side-effect at integrity time.
  Authors are responsible for keeping verifier commands
  read-only — this is not a new risk class.
- **Command-broken vs assertion-pending is indistinguishable.** A
  command that exits non-zero because the implementation isn't ready
  and a command that exits non-zero because the command itself is
  malformed both produce silent pass. The integrity gate cannot
  distinguish them. The bug surfaces when the implementer drops the
  `?` and the verifier runs at normal `loom gate verify` — the same
  command exits non-zero and surfaces as `verify-fail`. Delayed
  signal during the pending window, not silent forever.

The modifier is **self-cleaning**. It is modelled on Rust's
`#[expect(...)]` attribute, not `#[allow(...)]`: presence is silently
tolerated while the underlying condition holds; the moment the
condition resolves, the marker *itself* becomes the finding. The
implementer who lands the verifier must drop the `?` in the same
diff or the push gate refuses on `UnneededPendingMarker`
(recoverable: the finding mints into a remediation batch and the loop
re-enters until the cap exhausts; see *Integrity gate*). This
forces co-incidence between *"target now resolves"* and
*"marker now removed,"* so the spec tree never carries stale
markers past the molecule's push gate.

Lifecycle binding to plan → todo → loop:

- `loom plan` writes `[tier?](target)` when authoring a Success
  Criteria bullet whose verifier is not yet implemented. Applying the
  marker is part of the plan-stage Completeness check (see
  [*Plan-stage checks*](#plan-stage-checks) below).
- `loom todo` fans out beads from the spec diff as usual;
  pending-marked criteria are minted as ordinary tasks, with the
  integrity gate's self-cleaning behaviour as the only enforcement.
- `loom loop` implements the criterion. The implementer's diff drops
  the `?` from the annotation at the same time it lands the verifier;
  `UnneededPendingMarker` provides the structural enforcement that
  forces co-incidence.

**`[judge]` annotations are clickable links.** The path inside the
parentheses is read both by the gate (to dispatch a verifier) and by
markdown renderers (GitHub, VS Code, terminal viewers) when a reader
clicks the link. Two requirements compose to keep that click working:

1. **URL-fragment selector.** Shell-function selectors use `#fn`
   (standard markdown / URL fragment syntax), not `::fn`. A renderer
   sees `path#fn` as the same `path` it would for `path` alone, then
   scrolls to the `#fn` anchor; `path::fn` resolves to a literal
   filename ending in `::fn`, which 404s.
2. **Spec-relative path.** Paths are written relative to the spec
   file's own directory (e.g. `../tests/judges/x.sh#fn` from a spec
   in `specs/`). The renderer's relative-link resolution and the
   integrity gate's resolution share the same base, so a path that
   clicks correctly in a rendered spec also resolves on disk for the
   gate. Absolute paths are honoured as-is.

`::fn` selectors are accepted during migration; new annotations use
`#fn` so the click works.

#### Pending support in structured walker input

The per-annotation pending modifier above handles the common case:
one SC, one verifier target, dispatch-side skip when `?` is set.
**Sweeping walkers** — verifiers that read structured input from
the spec (the pinning-matrix walker reads templates.md's matrix
table; the surface-conformance walker reads harness.md's FR1
command-set; the anti-drift wire-format walker reads the canonical
partial path) and produce *per-element* findings from a single
dispatch — break that model: the SC-level `?` can suppress the
walker's dispatch entirely (only if every SC sharing the target is
`?`-marked) but cannot suppress individual elements the walker
reports inside one dispatch.

The structural fix: **a sweeping walker that reads structured
input from the spec MUST support pending element markers in that
input** — two markers, symmetric: `?` for *pending addition* (the
element will resolve to its assertion-side present value) and `~`
for *pending removal* (the element will resolve to its
assertion-side absent value). Same self-cleaning discipline as
per-annotation `[tier?]`: the marker silent-passes during the
pending window; the moment the underlying state catches up and
makes the marker stale, the walker fails so the author drops the
marker to its resolved value in the same diff.

**Walker contract** (additive — existing two-valued walkers extend
to four-valued):

For each element in the walker's structured input, the walker
checks the element's marker against the actual workspace state:

| Marker | Actual state | Outcome |
|---|---|---|
| present (e.g. `✓`) | present | silent pass |
| present | absent | walker failure (existing — assertion mismatch) |
| absent (blank) | absent | silent pass |
| absent | present | walker failure (existing — assertion mismatch) |
| `?` (pending addition) | absent | silent pass (pending — impl not yet caught up) |
| `?` (pending addition) | present | **walker failure** with `pending-marker-resolved` — author must drop `?` to the present marker (`✓` for matrix) in the same diff |
| `~` (pending removal) | present | silent pass (pending — impl not yet caught up) |
| `~` (pending removal) | absent | **walker failure** with `pending-marker-resolved` — author must drop `~` to absent (blank for matrix) in the same diff |

The walker continues to dispatch once and produce composite
results; the failure set excludes pending elements whose state
matches the pending direction (`?` + absent, `~` + present),
includes elements whose pending state has resolved.

**The marker is self-cleaning** — modelled the same way as the
per-annotation `[tier?]` modifier above. The author who lands the
impl that catches up to the matrix cell (`{% include %}` added for
`?`, removed for `~`) must drop the marker to its resolved value
in the same diff or the walker refuses on the resolved-state
failure. This forces co-incidence between *"impl caught up"* and
*"marker resolved,"* so the spec tree never carries stale pending
markers past the molecule's push gate.

**Concern token.** `pending-marker-resolved` — emitted by the
walker when a pending element's state has resolved against the
pending direction. Target variant depends on the walker emitting
it (the matrix walker uses `MatrixCell`, the surface walker uses
`SurfaceElement`, etc. — each sweeping walker defines a target
variant naming the specific element that should be resolved).
Routes to remediation bead (not clarify) — the resolution is mechanical,
not judgment-requiring.

**Adoption convention.** Every sweeping walker added to loom MUST
support `?` and `~` in its input from day one. Retrofitting
pending-marker support to existing walkers (the matrix walker is
the first case) is a walker-implementation change tracked as an
ordinary `loom loop` bead per the planning session that surfaces
the need.

### Runners — per-language batched dispatch

**Runners, not verifiers, are the dispatch unit.** A runner executes
one batch of annotations in a single subprocess. Per-language
batching avoids the "process per test" cost that dominates wall-clock
on non-trivial specs. One tier is carved out: `[system]` is
runner-owned for resolution and input-query, but its execution stays
per-annotation (see *Execution* below) — system verifiers are slow and
self-contained, so batching them does not pay.

The dispatcher's job:

1. Collect all in-scope annotations (per *Verifier inputs* + the
   scope flag's input set, intersected).
2. Group by which runner matches them.
3. For each runner with a batch template, build one command, spawn
   once, parse per-target verdicts from the output — except `[system]`,
   whose matched runner resolves inputs but still spawns one subprocess
   per annotation (see *Execution* below).
4. For unmatched annotations, fall back to per-annotation spawn.

**Schema: `[runner.<tier>.<name>]` in `<workspace>/loom.toml`.**
Each runner declares how to recognise its annotations, how to format
each target, how to join into a batch, how to parse per-target
results, and where to run from.

| Field | Purpose |
|---|---|
| `match` | Regex (PCRE-compatible) over the annotation's target string. Annotations whose target matches are dispatched through this runner. Capture groups are referenced by `{capture_N}` in `target`. Optional — when omitted, this runner is the default for the tier. |
| `command` | Command-line template. `{filter}` or `{targets}` substitute the joined-target string; `{capture_N}` substitutes a regex capture from the matched target. |
| `target` | Per-target template applied to each matched annotation before joining. References `{name}` (full target) or `{capture_N}` (capture groups from `match`). |
| `join` | String inserted between formatted targets to build `{filter}` / `{targets}`. |
| `parse` | Named built-in parser (see below) that extracts per-target verdicts from the runner's stdout. |
| `cwd` | Repo-relative directory to run the command from. Override the tier-default cwd. |
| `inputs` | Optional command template for the runner's **input-query** — how it asks its verifiers to self-report inputs, with `{print_inputs}` marking where the `--print-inputs` flag lands in the verifier's own argv (omit the placeholder and the flag is appended after the verifier's own arguments). Declaring `inputs` opts the runner into the input-query protocol: its verifiers MUST emit a well-formed inputs document or the gate raises `inputs-protocol-error` (see *Verifier inputs*). Omitted → the runner's annotations fall to the conservative always-run default — no precision, no protocol enforcement. |

**Built-in parsers** ship with loom — consumers add new runners that
emit one of these formats, rather than authoring custom parsers:

- `libtest-json` — Rust `cargo test`/`nextest` `--message-format`
  output: one event per test with `name` + `outcome`.
- `junitxml` — JUnit-XML reports (pytest, others). Parses
  `<testcase>` elements for pass/fail and message.
- `nix-build-status` — `nix build`'s per-derivation success/failure
  output.
- `json-lines` — one `{"target":"<name>","pass":bool,"evidence":"<msg>"}`
  per line on stdout. The simplest format for consumers writing
  custom batched runners: emit one line per target.
- `exit-code` — single per-runner verdict from the process exit
  code. Only useful for non-batched runners (one annotation per
  invocation).

**Tier-default cwd.** A `[runner.<tier>]` block (no `.<name>` suffix)
sets the default cwd for unmatched annotations in that tier:

```toml
[runner.check]
cwd = "loom"  # default cwd for all [check] annotations
```

Resolution order when spawning a command:

1. The matched runner's `cwd` field, if set.
2. Else the tier's default `cwd` (`[runner.<tier>] cwd = "..."`), if set.
3. Else repo root (`.`).

**Loom-the-library ships defaults** for the common toolchains —
nextest for `[test]` if a `Cargo.toml` is detected, nix for
`[system]` derivations, pytest if a `pyproject.toml` is detected.
Consumers extend or override in `<workspace>/loom.toml`. **Loom-
the-library has no privileged knowledge of any consumer's layout** —
the defaults are heuristics for common shapes, not assumptions.

**Runner-owned resolution, invocation, and input-query.** A runner
that `match`es an annotation **owns** that annotation end to end —
loom never falls back to parsing the annotation's argv for it:

- **Resolution.** The annotation resolves (integrity gate, direction
  1) because a runner claims it, not because its first token is on
  PATH. The `tokens[0]`-on-PATH check is the fallback for annotations
  no runner matches. Registering a runner — rather than wrapping logic
  in `sh -c "…"` so `tokens[0]` happens to be `sh` — is what earns
  precise scoping, batched dispatch, and loud input-query errors; the
  `sh -c` wrapper defeats all three and is no longer the only way to
  satisfy resolution.
- **Invocation.** The `command` template is the single definition of
  how the verifier is spawned. Loom does not reconstruct the command
  by token-splitting the annotation; `--print-inputs` (and every other
  argument) lands where the template places it, so a `cargo run -p
  loom-walk -- {targets}` runner queries the walk, never `cargo`.
- **Input-query.** Inputs come from the runner's `inputs` query (per
  *Verifier inputs* § Input-query protocol). For the batched tiers
  (`[check]`, `[test]`, `[judge]`) discovery is batched: one query spawn
  returns the per-target map for the whole matched group. `[system]` is
  the same exception as for execution (below): a runner match resolves
  its inputs but discovery stays per-annotation — one query spawn per
  `[system]` annotation. Discovery thus batches exactly where execution
  batches, so the parity invariant (§ Verifier inputs → Input-query
  protocol) holds for a runner-matched `[system]` group without a
  carve-out.
- **Execution.** For the batched tiers (`[check]`, `[test]`, `[judge]`),
  matched annotations batch into one subprocess per runner (the
  dispatcher's step 3 above); per-annotation spawn is only the unmatched
  fallback. `[system]` is the exception: a runner match resolves its
  inputs (above) but execution stays per-annotation — one subprocess per
  `[system]` annotation, matched or not, because system verifiers are
  inherently slow and self-contained, so batching does not help.

Unmatched annotations keep literal-command semantics — `tokens[0]`
resolution, heuristic input extraction, conservative always-run, no
protocol enforcement. The runner-owned path is the opt-in to
precision; the literal path is the floor.

#### Verifier inputs

A verifier's inputs are the **files it examines** — the gate
intersects them with a scope's input set to decide whether to run
the verifier: it runs iff `inputs ∩ scope input set ≠ ∅`. Inputs are
a **derived property of the verifier**, computed from the same
definition that does the verifying — never a parallel,
hand-maintained list that can drift from what the verifier actually
reads.

The wire format is a list of **gitignore-style glob patterns
relative to repo root**. How the gate derives them depends on
verifier kind:

| Verifier kind | Source of inputs |
|---|---|
| `[test](name)` | Test-framework metadata. For Rust: walk `cargo metadata`, resolve the test's owning crate, resolve the crate's source dirs. For pytest: pytest's collection output. For other frameworks: `<workspace>/loom.toml` `[runner.<tier>] inputs_for_test = "<command>"`. |
| `[check]` / `[system]` / `[judge]` whose target resolves to a **script or binary supporting the input-query protocol** | The verifier reports its own inputs: `<target> --print-inputs <remaining-argv>` prints JSON `{"inputs": ["glob1", "glob2"]}` to stdout (for `[judge]`, the remaining argv is the `#fn` selector — see *Judge collect mode*). |
| `[check]` / `[system]` — heuristic | Path extraction from genuine command tokens. `grep -q 'X' path/to/file` → `path/to/file`; `cargo test -p mycrate --lib testname` → `mycrate`'s sources via cargo metadata. Only tokens that are the verifier's own command arguments — never a guess at what a script reads internally. |

**Input-query protocol.** The `--print-inputs` query is issued
through the verifier's runner / command template — **never by
prepending the flag to the command's first token.** The template
decides where the flag lands, so a `cargo run -p loom-walk -- foo`
verifier is queried as the walk's own argument (after the `--`
boundary), not as an argument to `cargo`, and a `sh -c "<script>"`
verifier is queried by running the script, not by token-scanning for
a path. Two response shapes:

- **Single-target** — `{"inputs": ["glob", ...]}`, the inputs for the
  one target queried.
- **Batch** — `{"inputs": {"<target>": ["glob", ...], ...}}`, a
  per-target map. A runner that batches *execution* (one subprocess
  for many targets — see *Dispatch — per-tier process model*) reports
  inputs the same way: **one query spawn learns the inputs for its
  whole matched group**, never one spawn per target. Discovery
  batches exactly where execution batches, so scoping a large tree
  costs no more processes than running it.

**Target resolution.** A `[judge]` target is located by
selector-stripping + spec-relative resolution: a `#fn` / `::fn` /
`::attr` selector is stripped before the on-disk lookup, and a
relative path is joined against the annotation's spec-file directory
(not the repo root), matching the markdown renderer's relative-link
resolution. The integrity gate and the input resolver share **one
helper** for this, so the existence check and the collect-mode
invocation cannot disagree about where the judge script lives. This
is the deterministic resolution of a target that genuinely *is* a
path. A `[check]` / `[system]` target is *not* path-resolved this
way: it resolves by **runner match** — the matching runner owns the
annotation end to end (per *Runners*) — or, when no runner matches,
by the `tokens[0]`-on-PATH fallback. Its inputs come from the matched
runner's template (per *Input-query protocol*), never from scanning
its argv to guess which token is a file.

**Judge collect mode.** A judge script reports per-function inputs by
running the function in a **collect mode** rather than evaluating it:
`<script> --print-inputs <fn>` defines `judge_files` to *record* its
path arguments and `judge_criterion` (and any LLM call) as a no-op,
runs `<fn>`, and emits the recorded paths as `{"inputs": [...]}`.
Invoked with **no** `<fn>` the script emits the batch map for every
rubric it defines (`{"inputs": {"<fn>": [paths], ...}}`) in a single
spawn, so the gate learns one script's whole judge set at once. The
`judge_files` calls a rubric already makes are therefore the
**single source of truth** for that judge's inputs — per-function,
with no separate header to maintain or drift. This requires judge
scripts to be executable with the loom judge-harness preamble (which
supplies `judge_files` / `judge_criterion`); a judge whose collect
mode errors or emits a malformed inputs document is a loud finding,
not a silent fallback (see *Inputs-protocol error*).

**Spec-section auto-include.** The spec section the annotation lives
in is *always* part of the verifier's inputs — added automatically,
never declared. Editing the spec section re-runs the verifier. The
auto-include is an *additional* input, not the resolution floor:
when a verifier reports no inputs of its own, the gate does not
narrow it to the spec section alone (see *Conservative default*).

**Conservative default.** A verifier that reports no inputs of its
own — no test-framework metadata, no `--print-inputs` support, no
heuristic path token — **always runs** under every scope. Inputs are
an optimization that lets the gate *skip* verifiers it can prove are
unaffected; an undeterminable input set is never grounds to skip.
Incremental skipping must never silently drop a verifier that should
have fired, so "inputs unknown" resolves to *run*, not to *narrow to
the spec section*. Precision is opt-in (via `--print-inputs` or
`[test]` framework metadata); imprecision costs wasted work, never a
missed verifier.

**Inputs-protocol error.** Reporting inputs is opt-in, and the opt-in
is an **explicit signal, not a guess.** A verifier has opted in when
loom owns its input-query contract:

- a `[judge]` — the harness preamble guarantees `--print-inputs <fn>`
  is a real code path; or
- a `[check]` / `[system]` matched by a runner that declares an
  `inputs` query (see *Runners*).

An opted-in verifier that exits non-zero or emits a malformed inputs
document is a loud `inputs-protocol-error` finding (see *Concern
tokens and target variants*) — deterministic, emitted by the
integrity gate during `loom gate verify` / `check`, exiting non-zero
at the push gate and minting as a remediation at tree scope. This is the
integrity gate's fourth direction; see *Integrity gate*, Direction 4.
Because the
opt-in is explicit, loudness never mis-fires: a verifier whose
contract loom does *not* own — an unregistered literal command, or a
runner with no `inputs` query — falls through to the conservative
always-run default, **silently**. The gate never faults a `grep` or
`nix` invocation for declining a protocol it never opted into. (A
well-formed empty `{"inputs":[]}` from an opted-in verifier is a
deliberate narrow, honoured as-is — not an error.)

**Repo-agnostic.** The `--print-inputs` convention works for any
script or binary in any language, and the `[runner.<tier>]
inputs_for_test` config knob handles non-default test frameworks —
loom-the-library imposes no layout of its own.

Spec annotations stay **clean** — `[tier](target)` and nothing else.
No inline metadata, no HTML-comment companions, no syntax extensions,
**and no in-script `# loom-inputs:` header** — a verifier reports its
inputs by executing, not by carrying a comment a reader must keep in
sync. The reporting mechanism lives in the verifier's own definition
(test metadata, `--print-inputs`, command arguments), never beside
the annotation and never as a parallel declaration.

### Verifier-runner contract

Every verifier — whether `[check]` command, `[system]` command, or
the runner invoked by batched dispatch — is a subprocess that
conforms to:

- **Input:** env vars (`LOOM_FILES=<paths>` for `--files` runs,
  `LOOM_SPEC=<label>`, etc.) plus argv from the annotation's command
  string.
- **Output:** a JSON line on stdout matching the typed-verdict
  shape — `{"pass": bool, "evidence": "<message>"}`. Batched runners
  emit one such line per target via the `json-lines` parser, or use
  one of the other built-in parsers (`libtest-json`, `junitxml`,
  `nix-build-status`).
- **Exit code:** `0` for pass, `1` for fail, `2` for dispatch error
  (unknown verifier, command not found, missing prerequisite).

This works for any language. The contract is process-shaped, not
language-shaped.

**Exit code 2 is a fail at the push gate.** Dispatch errors — a
spec annotation referencing a walk that doesn't exist, a binary
that isn't on PATH, a command with a missing flag — produce exit
code `2`. The gate treats this as a hard fail (not a skip): the
verifier the spec is claiming exists, and the gate cannot confirm
it did anything. The push gate (FR9) refuses on any verifier exit
≠ 0, including dispatch errors. This closes the failure mode where
a spec asserts `[check](cargo run -p loom-walk -- foo_bar)` for a
walk `foo_bar` that nobody implemented — exit 2 → push refused →
the missing implementation surfaces immediately.

**Fallback for non-conforming verifiers.** Bare `grep -q`, `cargo
test`, `nix build`, and similar shells that don't emit a JSON
verdict line still satisfy the contract via their exit code alone:
the dispatcher interprets exit 0 as `pass=true` (stdout surfaced as
evidence), exit 77 as a skip (per the GNU test-suite /
`AM_TESTS_ENVIRONMENT` convention — the verifier reports a missing
prerequisite rather than a real failure), and any other non-zero
exit as `pass=false` (stderr surfaced as evidence). The third
verdict propagates through dispatch as `skipped=true` on
`VerifierVerdict` and persists as `Verdict::Skipped` in the status
cache, so a verifier that legitimately cannot run does not count
as a failure against the molecule. Verifiers that emit a JSON line
are preferred — the explicit evidence string clicks straight to the
violation site — but the exit-code fallback keeps simple
presence/absence checks viable without wrapping each one in a Rust
walk.

### `--files` scope handling

For file-filterable batched execution paths, the gate filters
annotations to those whose scope intersects `--files` before issuing
the batched invocation:

- `[test]`-tier scope = files in `crate(test)` ∪ files in
  `crate(test)`'s transitive dependencies (Rust; computed via
  `cargo metadata`). Other toolchains supply analogous mappings.
- Runner-matched `[check]` scope = the matched runner's per-target
  `inputs` query result, plus the spec-section auto-include. There is
  no cargo-metadata crate map for `[check]`; a matched runner that has
  no `inputs` query falls back to the conservative always-run default
  before the batch is formed. `[judge]` dispatch is batched but not
  file-filtered under `--files`.
- For non-batched execution paths (`[system]` and unmatched fallback
  annotations), the gate passes `LOOM_FILES` as env and the verifier
  decides whether to filter. Most verifiers can be dumb (run the same
  way regardless); walks that benefit from scope filtering read the env
  var.

### Test-tier silent-zero-match

`cargo test -- some_name` and equivalents in other runners exit 0
silently when no test matches the filter. The gate sniffs known
runners (`cargo test`, `cargo nextest`, `pytest`) and post-processes
output to detect zero-match cases, failing the run with a clear
error. Consumers using unrecognised runners must ensure their
runner fails on zero-match.

## Integrity gate

The deterministic gate that verifies the annotations themselves
resolve. Runs as part of `loom gate check`. Four directions:

1. **Forward — every annotation's target is valid for its tier.**
   - `[check](target)` and `[system](target)`: the target resolves
     via a matching runner (`[runner.<tier>.<name>] match`), or — when
     no runner claims it — its first token resolves on PATH or as a
     file in the repo (best-effort; dynamic commands may resolve only
     at runtime). Runner-match is the primary path; the `tokens[0]`
     check is the unregistered-command fallback.
   - `[test](path)`: the path resolves to a `#[test]` /
     `#[tokio::test]` / proptest function (or language equivalent)
     in the consumer's workspace, via the consumer's toolchain
     metadata.
   - `[judge](path)`: the path resolves to a file on disk.

   The pending modifier `?` (see [*Pending modifier*](#pending-modifier)
   above) flips the per-annotation outcome: a `[tier?](target)` whose
   target does not resolve passes silently; one whose target *does*
   resolve emits an `UnneededPendingMarker` finding, naming the spec,
   line, and target so the implementer can drop the `?` in the same
   diff that lands the verifier.

2. **Stub-pointing — annotations whose verifier body invokes the
   `_pending_stub` sigil are flagged** (`StubTestFunction`). A stub
   means the criterion has no real evidence; the deterministic gate
   flags it without waiting for `loom gate review`'s
   verifier-honesty rubric. The pending modifier suppresses
   `StubTestFunction` the same way it suppresses
   `UnresolvedAnnotation`; once the test body becomes non-stub the
   modifier triggers `UnneededPendingMarker` for that annotation.

3. **Atomic acceptance — each criterion carries exactly one
   annotation.** Two annotations on one criterion is a flag
   (ambiguous pass/fail when one passes and the other fails).
   N→1 sharing is allowed (multiple criteria pointing at the same
   verifier). Atomic acceptance is structural and **not**
   suppressible by `?` — having two annotations on one criterion is
   wrong regardless of either's resolution state.

4. **Inputs-protocol honesty — an opted-in input-query must honour
   its contract** (`inputs-protocol-error`). A
   verifier opts in when loom owns its query: a `[judge]` (the harness
   preamble guarantees `<script> --print-inputs <fn>`) or a `[check]` /
   `[system]` whose
   runner declares an `inputs` query (see *Runners*). An opted-in
   query that exits non-zero or emits a malformed inputs document is
   unambiguously broken and the gate flags it. This is the loud
   counterpart to the *Conservative default*: a verifier whose
   contract loom does not own falls through to always-run silently, so
   no `grep` / `nix` command is ever mis-flagged. The pending modifier
   suppresses `inputs-protocol-error` the same way it suppresses
   `UnresolvedAnnotation` — a `[tier?]` annotation has no verifier yet
   to hold to the protocol.

Failure output (one per finding):

- `<spec>:<line>: annotation [tier](<target>) — does not resolve`
- `<spec>:<line>: criterion carries N annotations, expected 1`
- `<spec>:<line>: annotation [tier](<target>) points at stub function`
- `<spec>:<line>: annotation [tier?](<target>) is now resolved — drop the ? marker`
- `<spec>:<line>: annotation [tier](<target>) — input-query errored / emitted a malformed inputs document`

**Integrity findings at the push gate are recoverable up to the
molecule's iteration cap.** When deterministic push verification over
`origin/<integration-branch>..HEAD` produces one or more
`UnresolvedAnnotation`, `StubTestFunction`, `UnneededPendingMarker`, or
`inputs-protocol-error` findings within the actual push range, the
verdict gate normalizes each into a typed `Finding` per the mapping in
*Findings and Minting — Concern tokens and target variants* and merges
them into the molecule's deferred remediation set (per *Deferred
remediation processing*). The findings coalesce into one remediation
batch per lead-spec / concern family, carrying all integrity findings
the audit emitted for it. The push is refused for this iteration, the
iteration counter is incremented, `loom gate mint -m/--molecule`
promotes the batch, and the outer loop re-enters so the worker can
address it.

**Cap-exhausted fallback.** The recovery branch is bounded by the
molecule's iteration cap. When the counter exhausts, the verdict
gate falls back to the terminal escalation: `loom:clarify` on the
molecule's epic with **one composed `## Options — …` block** per
the *Options Format Contract*. The composition rule is mechanical:

- For each integrity finding kind present in the molecule's
  findings (in the order they appear below — `UnresolvedAnnotation`,
  `StubTestFunction`, `UnneededPendingMarker`), emit one
  `### Option N` entry drawn from the **primary (Option 1) of that
  kind's per-kind auto-options template** below, scoped to the
  affected findings (e.g. *"Option 1 — Drop the `?` markers at
  specs/templates.md lines 913, 920, 925"*).
- Close the block with one final `### Option N` for
  *"Mixed resolution via `loom inbox chat`"* — the escape hatch when
  the operator needs different resolutions across findings or wants
  options beyond each kind's primary.

This preserves the Options-Format-Contract invariant of one block
per clarify bead while keeping per-kind resolution paths visible
to the operator.

**Worker authority on the recovery branch.** Findings are not
classified as self-fixable in the driver; the worker is the
authority on whether one turn can resolve every finding in the
batch. A worker that cannot resolve the batch emits `LOOM_CLARIFY`
from its own dispatch, which routes through the standard per-bead
clarify path — the iteration cap is the backstop for both "worker
keeps failing on the same finding" and "findings are intrinsically
clarify-shaped."

**Per-kind auto-options templates.** The templates below are the
building blocks the composition draws from. Two consumption sites:

- **Recovery branch (cap not exhausted).** The worker's remediation
  batch description embeds the kind-appropriate template alongside
  each finding as a suggested mechanical resolution.
- **Cap-exhausted fallback.** The gate composes one primary option
  per present kind from these templates per the rule above.

**Auto-generated options for `UnresolvedAnnotation`.** The gate has
enough information (target string, tier, spec location) to draft
options for the human:

- *Option 1* — Implement the missing verifier (walk / test / judge /
  system check) at the expected path.
- *Option 2* — Retarget the annotation to an existing verifier
  (gate lists nearest matches by name).
- *Option 3* — Mark the annotation pending with `?` if the verifier
  is intentionally deferred to a follow-on bead — the integrity gate
  will then silently accept it until the implementing diff drops the
  `?` and the target resolves in the same commit.
- *Option 4* — Remove the criterion at `<spec>:<line>` if it's
  superseded or out of scope.

**Auto-generated options for `StubTestFunction`.** Similar shape:

- *Option 1* — Implement the test body, replacing the
  `_pending_stub` sigil.
- *Option 2* — Retarget the annotation to a non-stub verifier.
- *Option 3* — Mark the annotation pending with `?` if the
  implementation is intentionally deferred (same self-cleaning
  semantics as for unresolved targets).
- *Option 4* — Remove the criterion if the work isn't planned.

**Auto-generated options for `UnneededPendingMarker`.** The marker
is stale; the implementation has caught up to the claim:

- *Option 1* — Drop the `?` from `[tier?](<target>)` at
  `<spec>:<line>` so the annotation reads `[tier](<target>)`. This
  is the expected resolution and almost always the right one.
- *Option 2* — If the resolution is incidental (the target name
  collides with an unrelated symbol now visible in the workspace),
  retarget the annotation to the actual intended verifier and keep
  `?` until *that* one resolves.

The integrity gate is itself a `[check]`-tier verifier (its own
spec criterion annotates back to its implementation), so every
`loom gate check` run includes a self-test of the gate's resolution
logic.

## Status cache

`loom gate status` reads criterion evidence from the unified
`.loom/cache.db` cache and prints a fast report. (Bare `loom gate`
shows the subcommand help — see *Commands* above.) Every subcommand
that runs verifiers or the LLM rubric writes to the cache as it runs —
`loom gate verify`, `loom gate review`, `loom gate audit`, the tier
subcommands (`check` / `test` / `system` / `judge` / `rubric`), and
`loom gate mint` (via its embedded verify and rubric walks). There is no
separate `.loom/gate-cache.sqlite`.

**Cache contents per criterion:**
- typed `CriterionId` (requirement identity) and current annotation
  snapshot
- last-run timestamp and commit hash
- pass / fail / skipped verdict (`skipped` covers scope-filter
  exclusion and verifier-reported prerequisite gaps via exit 77)
- evidence string from the verifier's JSON output

**Cache schema** is part of `.loom/cache.db` in
[harness.md](harness.md). One row per criterion, indexed by typed
`(SpecLabel, CriterionId)`. If the current spec file's annotation differs
from the cached annotation for the same criterion id, todo renders
`EvidenceState::StaleAnnotation` rather than reusing stale pass evidence.

**Report contents** when `loom gate status` runs:
- per-spec criterion counts: total, annotated, un-annotated
- last-run summary per tier: when, pass/fail counts, currently-failing criteria
- annotation health: broken annotations (target doesn't resolve),
  stale runs (cache older than N days)

**Hard target:** report renders in <500ms on a corpus of arbitrary
size. A self-test asserts this — the cache implementation, not the
corpus, is what determines the latency.

## Options Format Contract

Whenever the gate (or, in practice, the reviewing agent acting on
behalf of the gate) raises `loom:clarify` — for an invariant clash,
for a verifier-honesty concern with multiple resolution paths, or
for any review-time decision the user must pick from — the bead body
presents the candidate paths as a structured markdown block that
`loom inbox view` can render and `loom inbox chat` can use as structured
resolution context:

```markdown
## Options — <one-line summary of the decision>

### Option 1 — Preserve the invariant
<body explaining what reworking the change to preserve the invariant
would look like, including the cost>

### Option 2 — Keep the change on top of the invariant
<body explaining what carrying the contradiction would entail —
which spec section to record the debt in, what cleanup follow-up
to file>

### Option 3 — Change the invariant
<body explaining what updating the spec would entail — which
invariant to weaken or remove, what code realignment would follow>
```

`loom inbox` consumes this format without a host-side option picker:

- **List mode** (`loom inbox`): the `## Options — <summary>` line is
  rendered as the bead's SUMMARY column.
- **View mode** (`loom inbox view <N>` / `loom inbox view -b <id>`): the
  full block is rendered to the user with each `### Option N` heading.
- **Chat mode** (`loom inbox chat`): the block is rendered as structured context
  for the human and chat agent. The chat agent records any authorized decision
  through Beads; Loom does not infer executable actions from option prose.

A clarify bead can present fewer or differently-framed options when
the decision warrants — the format is `### Option <integer> —
<title>` for any integer ≥ 1. The summary line is always required.

**Three application paths, one shape requirement.** Three distinct
paths apply `loom:clarify` to a bead. All three require a
well-formed `## Options — <summary>` heading with at least one
`### Option <N> — <title>` subsection somewhere readable by `loom
inbox` (bead notes ∪ description). Each path has its own writer and
validator, but the *shape* of the options block and the *failure
mode* on absence are uniform:

| Path | Writer of the options block | Where the block lives | Validator | Failure mode |
|---|---|---|---|---|
| **Mint-from-finding** (worker phase emits `LOOM_FINDING` with a clarify-route token) | Rubric agent — embeds the block inside `evidence` | Mint extracts from `evidence` into the minted bead's description | `loom gate mint` (per *Deferred remediation processing* step 4) | Fall back to `loom:blocked` cause `clarify-without-options` |
| **Direct-emit `LOOM_CLARIFY`** (`loop` / `todo` worker emits the marker; target is the bead under dispatch for `loop`, or the `loom:todo` work epic for `todo` per [templates.md — Decomposition Discipline](templates.md#decomposition-discipline)) | The worker agent itself, via `bd update --notes` / `bd update --description` against the target before emitting the marker | The target bead/work epic's notes or description | Verdict gate (per [harness.md § Verdict Gate](harness.md#verdict-gate) marker definitions) | Fall back to `loom:blocked` cause `clarify-without-options` |
| **Existing-bead promotion** (chat agent in `loom inbox chat` upgrades a `loom:blocked` bead) | The chat agent, with human authorization | The bead's notes (added via `bd update --notes` before `bd update --add-label=loom:clarify`) | None — the chat agent has bd-write authority and the human authorizes each turn (per [harness.md § Inbox Modes](harness.md#inbox-modes)) | n/a (no automatic validation; if the chat agent skips the options write, the human catches it next turn) |

The structural enforcement at the chokepoint is what makes
"stranded clarify bead the chat-drafter cannot resolve"
unrepresentable for the two worker-phase paths — the agent either
provides a well-formed options block (clarify applied) or emits
`LOOM_BLOCKED` directly with a reason explaining why options cannot be
enumerated (no clarify ever applied). The existing-bead promotion path
is not subject to the chokepoint
because chat is human-authoritative.

**The gate does not scrape free-form stdout for `## Options` /
`### Option N` blocks.** Only the structured locations above carry
the canonical contract — `evidence` for mint-from-finding, bead
notes/description for loop/todo direct-emit and existing-bead paths.
Review clarifications use the mint-from-finding path; review prompts do
not direct agents to mutate bd state.

### Resolution lifecycle

The `## Options — <summary>` block lives on the target bead (in
notes or description, per the path table above) only from emit to
resolution. When `loom:clarify` is cleared by an inbox chat session's
`bd update --remove-label=loom:clarify`, the originating options block is
removed from wherever it lives (notes or description) in the same authorized
resolution update that records the human decision.

A single bead can receive multiple clarifications across its
lifetime — notably a `loom:todo` work epic, which hosts
decomposition-phase clarifies emitted by successive `loom todo`
invocations while the same pending fingerprint is being repaired. Without removal,
options blocks accumulate and `loom inbox` lists become ambiguous
about which block belongs to the currently active label.

For clarifies hosted on a **dedicated clarify bead** (created via
the mint-from-finding path above and closed during inbox chat), the removal is
moot — the whole bead is closed and the notes/description pass out of scope
with it. The lifecycle contract is load-bearing for the
**existing-bead promotion** path where the bead survives the
resolution.

## Output

The gate's output is a verdict (pass / hard-fail / clarify) plus any
flagged actions. Gate invocations also write JSONL evidence logs under
`.loom/logs/gate/`; `bd` issues and git commits remain the durable work
record.

- **Worker/per-bead deterministic failures** drive the existing recovery
  loop with `previous_failure` context. They do not produce Finding
  records or remediation batches.
- **Push-range rubric findings** route by their explicit `route` field:
  `blocking` findings refuse the push and create or reuse same-molecule
  remediation work, `deferred` findings merge into molecule-local
  `loom:deferred` beads, and `clarify` findings materialize one human-
  decision bead per finding hash. Suppressed rubric findings are
  reported in summaries but do not affect verdicts or bd state.
- **Tree-scope deterministic + unsuppressed rubric findings** (`mint
  --tree`) materialize as ready remediation batches under one standing
  remediation work epic for the run — grouped by lead-spec / concern
  family after per-finding dedup, but not parented under per-spec work
  epics. If no actionable child batch remains, no work epic is created.
- **Push-gate integrity findings** (per *Integrity gate*'s recovery
  branch) merge into the molecule's deferred remediation set and are
  promoted by `loom gate mint -m/--molecule <id>` during stabilization.
- **Clarify-route findings** (currently defaulted only by
  `invariant-clash`; future default-clarify tokens follow the same
  path automatically) mint as single-finding beads — one bead per
  finding, never bundled — carrying `loom:clarify` with the
  `## Options — …` block from the finding's `evidence` rendered into
  the bead's description per the Options Format Contract. The
  per-finding shape is load-bearing because `loom inbox` cannot consume
  a bead carrying multiple options blocks. Clarify-route findings whose
  evidence lacks a well-formed options block fall back to
  `loom:blocked` with cause `clarify-without-options` rather than
  minting a stranded clarify bead.

Past gate runs are persisted for observability, but *past passes don't
grant immunity from re-evaluation*. Conformance is a property of the
current code-spec pair, tree, config, and push range, not a historical
fact.

## Recovery

Per-stage flag handling:

- **Plan** — interview held until the spec is amended (claim
  surfaced, clash resolved, or explicitly out-of-scope'd). User
  authorisation required to ship a spec with unresolved gaps.
- **Worker / per-bead integration** — worker self-check failures are
  prompt-level feedback; driver post-integration deterministic failures
  roll back the integration and enter same-bead recovery with
  `previous_failure` rendered into the next prompt.
- **Push** — deterministic pre-push failures skip review, refuse the
  push, and create or reuse same-molecule remediation work. Review
  `route="blocking"` findings do the same. `route="deferred"` findings
  are stored on `status=deferred` / `loom:deferred` molecule
  remediation beads and are not returned by `bd ready` until
  stabilization promotes them. `route="clarify"` findings create or
  update one `loom:clarify` bead per finding hash; `loom inbox` resolves
  the clarify. Clashes never trigger fresh-agent retry of an already-
  integrated original bead.
- **Standing** — `loom gate mint --tree` walks the deterministic
  verifiers and the LLM rubric, materializes typed findings as ready
  remediation batches grouped by lead spec / concern family (plus
  single-finding clarify beads for any clarify-route findings) under one
  active standing remediation work epic while ensuring each owning spec
  has exactly one spec epic. If no actionable batch remains after
  suppression/dedup, it creates no work epic. Invariant clashes surface
  via `loom:clarify` on the minted single-finding clarify bead; resolved
  in the next `loom inbox` walk. See
  [*Findings and Minting*](#findings-and-minting) for the deferred
  remediation processing flow.

### Post-hoc recovery — when the push gate was skipped

**Use case.** A molecule's beads closed without `GateSuccess` being
constructed — e.g., a legacy run from before the type-shape
enforcement landed, or a manual `bd close` outside the gate. The
work shipped but was never audited; the codebase has unverified
divergence from the spec. The original push-stage range no longer
applies because HEAD and origin have moved on to subsequent work, and
reconstructing the old range would mix unrelated downstream commits.

**Canonical recovery path:** `loom gate mint --tree`. The standing-
safety-net scope is exactly what's needed — walk the full spec set
against the full implementation, no diff math, no dependence on a
still-valid `loom.todo_cursor`, with remediation beads grouped by lead
spec under one active remediation work epic as findings emerge.

```bash
loom gate mint --tree                  # walks every spec; mints one active
                                       #   remediation work epic when needed
loom loop                              # runs the active remediation epic
```

For inspection without minting, `loom gate audit --tree` runs the same
walk and prints findings to stdout without bd writes.

No explicit seeding step is required — mint ensures spec epics via the
lifecycle in [harness.md — Spec and Work Epic Lifecycle](harness.md#spec-and-work-epic-lifecycle)
and creates a single active work epic for actionable remediation.
Recovery is just the standing safety net exercised explicitly.

**Compositional safety.** The recovery flow's `loom loop` produces
`GateOutcome` per molecule — silent skip is structurally
unrepresentable (see [harness.md Loop Outcome Types](harness.md#loop-outcome-types)).
The worker-queue filter (harness.md FR1) prevents the agent from
receiving an epic as a worker task. Together, the conditions for
the original gate-skip class are structurally unreachable.

## Success Criteria

The gate's spec defines the verifier-annotation taxonomy, so these
criteria self-host — they use the same `[check]` / `[test]` /
`[system]` / `[judge]` annotations the rest of the spec defines. The
integrity gate's self-referential tests (under *Integrity gate — four
directions* below) pin that this self-hosting actually resolves: a
`[check]` annotation in `specs/gate.md` whose first token is on
PATH, and a `[judge]` annotation pointing at the gate's own
`src/integrity.rs`, both pass forward resolution.

### Annotation parsing

- Parser walks every `.md` file in the specs directory in lexical order
  [test](parse_walks_all_md_files_in_lex_order)
- Parser skips non-`.md` files in the specs directory
  [test](parse_skips_non_markdown_files_in_specs_dir)
- Parser aggregates criteria across multiple spec files into a single
  `ParsedSpecs`
  [test](parse_aggregates_criteria_across_files)
- Parser returns a typed read-directory error when the specs directory
  is missing rather than producing an empty result
  [test](parse_returns_read_dir_error_for_missing_directory)

### Integrity gate — four directions

Directions 1–3 (forward resolution, stub-pointing, atomic acceptance)
are covered by the criteria below. Direction 4 (inputs-protocol
honesty) is covered by the `inputs-protocol-error` criteria under
*Verifier inputs* below — opt-in resolution, opted-in query failure,
and conservative fall-through for unowned queries.

- **Forward — baseline.** A spec with all valid annotations yields no
  findings
  [test](parse_then_check_with_all_valid_annotations_yields_no_findings)
- **Forward — broken targets per tier.** Each tier flags its own
  broken-target shape: `[check]` first token absent on PATH, `[test]`
  path with no matching function, `[judge]` file absent
  [test](fixture_with_broken_target_per_tier_flags_each_one)
- **Forward — judge `#fn` selector.** A `[judge](script#fn)` target
  resolves when the leading script path exists; the `#fn` suffix is
  stripped before the on-disk check (per the *Verifier inputs* table's
  `[judge](script#fn)` row). `::fn` is accepted during migration but
  `#fn` is canonical because the URL-fragment shape is what markdown
  renderers click through to
  [test](forward_judge_accepts_script_with_hash_fn_selector)
- **Forward — judge spec-relative resolution.** Path resolution joins
  the relative target against the annotation's spec-file directory, not
  the repo root; absolute paths are honoured as-is. This matches the
  markdown renderer's relative-link resolution so a clickable
  `[judge](../tests/judges/x.sh#fn)` in `specs/foo.md` resolves to
  `tests/judges/x.sh` on disk
  [test](forward_judge_resolves_relative_to_spec_dir)
- **Forward — judge legacy `::fn` selector.** A `[judge](script::fn)`
  target still resolves during the `::` → `#` migration; the `::fn`
  suffix is stripped before the on-disk check
  [test](forward_judge_accepts_script_with_fn_selector)
- **Forward — system `::attr` selector.** A `[system](path::attr)`
  target (e.g. `[system](tests/unit.nix::eval-smoke)`)
  resolves when the leading path exists; the `::attr` suffix is
  stripped before the PATH / file check, matching the `[judge]` shape
  [test](forward_system_accepts_path_with_attr_selector)
- **Forward — test-tier missing function.** A `[test](cargo test …)`
  annotation whose test name does not match any function in the
  workspace is flagged
  [test](check_flags_cargo_test_annotation_with_missing_test_name)
- **Stub-pointing.** A `[test]` annotation whose body invokes the
  `_pending_stub` sigil is flagged as `StubTestFunction`
  [test](stub_pointing_test_annotation_flags_via_workspace_scanner)
- **Atomic-acceptance.** Two annotations on one criterion flags
  `MultipleAnnotations`
  [test](two_annotations_on_one_criterion_flags_atomic_acceptance)
- **End-to-end.** A specs directory containing both broken-target and
  multiple-annotation fixtures surfaces findings from both directions
  in one pass
  [test](end_to_end_specs_dir_check_combines_both_directions)
- **Verify lane — terminal.** Integrity findings exit non-zero from
  `loom gate check` and `loom gate verify` runs (the integrity gate is
  itself a `[check]`-tier verifier, so it fails the same way the
  per-annotation `[check]` dispatch does)
  [test](gate_check_fails_on_integrity_finding_for_unresolved_annotation)
- **Self-hosting — check tier.** The integrity gate accepts a
  `[check]` annotation in `specs/gate.md` whose first token
  resolves on PATH (closes the bootstrap concern: the spec that defines
  the taxonomy can carry its own annotations)
  [test](self_referential_check_annotation_resolves_against_integrity_gate_implementation)
- **Self-hosting — judge tier.** A `[judge]` annotation in
  `specs/gate.md` pointing at the integrity gate's own
  `src/integrity.rs` resolves
  [test](self_referential_judge_annotation_resolves_against_integrity_source_file)

### Integrity gate — pending modifier

- **Parser recognises `?` modifier — all tiers.** `[check?]`,
  `[test?]`, `[system?]`, `[judge?]` all parse; the parser populates
  a `pending: bool` on the resulting annotation
  [test](parse_recognises_pending_modifier_for_all_four_tiers)
- **Pending — unresolved target silently passes.**
  `[check?](missing-cmd)`, `[test?](missing::fn)`,
  `[system?](missing-system-cmd)`, `[judge?](missing/path.md)` all
  clear forward resolution with no finding
  [test](pending_marked_unresolved_target_yields_no_finding)
- **Pending — resolved target emits `UnneededPendingMarker`.** A
  `[check?](true)` (where `true` is on PATH) flags the stale marker,
  naming spec + line + target
  [test](pending_marked_resolved_target_yields_unneeded_pending_marker)
- **Pending `[test?]` — stub body silently passes.** The modifier
  suppresses `StubTestFunction` the same way it suppresses
  `UnresolvedAnnotation`
  [test](pending_marked_stub_test_body_yields_no_finding)
- **Pending `[test?]` — non-stub body emits `UnneededPendingMarker`.**
  Co-incidence with target resolution forces the implementing diff to
  drop `?` at the same commit that lands the real body
  [test](pending_marked_non_stub_test_body_yields_unneeded_pending_marker)
- **Atomic-acceptance — `?` does not suppress.** Two annotations on
  one criterion still flag `MultipleAnnotations`, whether either,
  both, or neither carries `?`
  [test](pending_modifier_does_not_suppress_atomic_acceptance_finding)
- **`UnneededPendingMarker` — terminal at push gate.** Surfaces
  alongside `UnresolvedAnnotation` and `StubTestFunction` per the
  *Findings and Minting* emit-shape table
  [test](unneeded_pending_marker_is_terminal_at_push_gate)
- **`unneeded-pending-marker` — auto-generated options.** `mint`
  emits a `## Options — …` block whose Option 1 is "drop the `?`",
  per *Integrity gate* above
  [test](mint_emits_drop_marker_option_for_unneeded_pending_marker)

### Standing-safety-net bonding

- `loom gate mint --tree` resolves each finding's bonded specs via typed
  `bonds` / target data, ensures exactly one spec epic exists for each
  bonded indexed spec (per [harness.md — Spec and Work Epic
  Lifecycle](harness.md#spec-and-work-epic-lifecycle)), auto-creates
  and immediately closes missing metadata-only spec epics, and refuses
  duplicate spec epics before creating remediation work
  [test](mint_tree_scope_resolves_bonded_spec_epics_without_per_spec_work_epics)
- `loom gate mint --tree` creates exactly one remediation work epic for
  all actionable tree-scope batches in a run, parents every fix-up,
  blocked-clarify, and clarify bead under it, applies `loom:active` to
  that epic, clears `loom:active` from any previous work epic, names the
  active epic id and `loom loop` command in the summary, and writes no
  current-spec or pointer-table state
  [test](mint_tree_scope_mints_single_active_work_epic_for_all_actionable_batches)
- `loom gate mint --tree` creates no remediation work epic and leaves
  `loom:active` unchanged when no unsuppressed actionable finding remains
  after suppression, dedup, and structural validation
  [test](mint_tree_scope_without_actionable_findings_creates_no_epic)
- `loom gate audit --tree` is inspection-only: it walks the same
  rubric `mint --tree` walks and prints findings to stdout, but
  produces zero bd writes
  [test](audit_tree_scope_makes_no_bd_writes)

### Findings and Minting

- `loom gate mint` refuses to run when `LOOM_INSIDE=1`, exiting
  non-zero with a deterministic error message and producing no bd
  writes
  [test](mint_refuses_when_loom_inside_env_is_set)
- The walk emits `LOOM_FINDING: <json>` records on stdout, one
  JSON object per finding, streamed as findings are identified
  (not batched at end-of-walk). The JSON shape is `{"token": ...,
  "route": "blocking|deferred|clarify", "bonds": [...], "target":
  {"kind": ..., ...}, "evidence": ...}`
  [test](mint_walk_emits_loom_finding_json_lines_streamed_per_finding)
- Long evidence may include raw line breaks inside JSON strings; the
  driver normalizes them before typed validation and preserves the
  resulting evidence line breaks
  [test](raw_multiline_evidence_is_normalized_before_strict_validation)
- The review walk terminates with exactly one of `LOOM_COMPLETE`,
  `LOOM_CONCERN: {"summary": "..."}`, `LOOM_RETRY`, or `LOOM_BLOCKED`;
  `LOOM_BLOCKED` includes a reason explaining why no options can be safely
  surfaced, review clarifications are `route="clarify"` findings with Options
  in `evidence`, and a walk that emits `LOOM_FINDING:` records without a
  terminal marker fails the mint invocation with non-zero exit
  [test](mint_walk_without_terminal_marker_fails_run)
- A walk that terminates with `LOOM_RETRY` (review itself could not
  run for environmental reasons) routes to recovery cause
  `agent-retry` per [harness.md § Verdict Gate](harness.md#verdict-gate);
  consumes one `[loop] max_retries` slot; exhaustion escalates to
  `loom:blocked` with cause `retry-exhausted`
  [test](retry_marker_routes_to_agent_retry_recovery_cause)
- `LOOM_CONCERN:` payload parses as JSON `{"summary": "<non-empty
  string>"}` via the same `serde_json` pipeline that consumes
  `LOOM_FINDING:` records; the parsed summary becomes the verdict-log
  entry for the walk
  [test](concern_payload_parses_as_json_with_summary_field)
- Parse failures on the `LOOM_CONCERN:` payload — invalid JSON, missing
  `summary` field, empty `summary` string — surface as
  `RecoveryCause::BadWalk(BadWalk::Concern { payload })` carrying the
  literal post-marker text so the recovery prompt can quote it back
  to the agent
  [test](concern_malformed_payload_routes_to_bad_walk_concern_with_literal_payload)
- A walk that emits `LOOM_CONCERN:` with zero preceding `LOOM_FINDING:`
  records surfaces as `RecoveryCause::BadWalk(BadWalk::ConcernWithoutFindings
  { summary })` — concern claimed without enumeration
  [test](concern_without_streamed_findings_routes_to_badwalk_concern_without_findings)
- A walk that streams one or more `LOOM_FINDING:` records and terminates
  with `LOOM_COMPLETE` surfaces as `RecoveryCause::BadWalk(BadWalk::
  FindingsWithoutConcern { finding_count })`
  [test](findings_streamed_with_complete_terminator_routes_to_badwalk_findings_without_concern)
- The wire-format anti-drift verifier (a `[check]`-tier audit) scans
  every file under `crates/loom-templates/templates/` for the literal
  substrings `LOOM_CONCERN:` and `LOOM_FINDING:` and fails if they
  appear in any file other than `partial/findings_walk.md`. Bare-prose
  mentions without the colon (e.g. *"the `LOOM_CONCERN` marker"*) are
  unaffected
  [check](cargo run -p loom-walk -- template_wire_format_restatement)
- The anti-drift verifier accepts the canonical layout: with
  `LOOM_FINDING:` / `LOOM_CONCERN:` substrings present only in
  `partial/findings_walk.md`, the walk reports zero violations
  [test](anti_drift_verifier_passes_canonical_partial_layout)
- The anti-drift verifier fails a fixture where a template restates
  the wire-format outside the canonical partial — e.g. injecting
  `LOOM_FINDING:` into `review.md` directly — naming the offending
  file and line
  [test](anti_drift_verifier_fails_fixture_with_restated_wire_format)
- The driver parses `LOOM_FINDING:` JSON payloads via `serde_json`
  into typed `Finding` records; the `target` field deserializes as
  an internally-tagged enum whose variant is selected by `kind`,
  validated against the `token`'s expected variant
  [test](mint_parses_loom_finding_json_into_typed_record_with_tagged_target)
- A malformed `LOOM_FINDING:` record — invalid JSON after raw string
  line-break normalization, unknown token, unknown spec, target variant
  mismatching token, or unresolved target content — fails the mint
  invocation with a typed parse error naming the offending record's
  start line; no silent skip
  [test](mint_malformed_loom_finding_fails_run_with_typed_error)
- The driver computes a versioned lower-kebab finding id from each
  validated typed finding; evidence text, options prose, line numbers,
  batch size, sibling batch membership, current bd parent, and `bonds`
  ordering do not affect the id
  [test](mint_computes_versioned_finding_id_excluding_volatile_context)
- The driver computes a compact finding hash from the finding id and
  refuses on hash collision instead of merging two different ids under
  one `finding:<hash>` label
  [test](mint_refuses_finding_hash_collision)
- `StyleRule` targets include a concrete subject in addition to
  `rule_id`; a rule-id-only style target is rejected as too broad for
  dedup or suppression
  [test](style_rule_finding_requires_concrete_subject)
- Mint dedups by `finding:<hash>` labels across live statuses; one
  live result skips that finding, zero live results allows minting,
  and more than one live result refuses the mint run as a structural
  violation
  [test](mint_dedups_per_finding_hash_label_across_live_statuses)
- Closed beads carrying `finding:<hash>` suppress automatic reminting
  only within the same owning molecule; closed matches outside the
  molecule are historical context only and do not suppress newly
  observed current findings
  [test](closed_finding_hash_label_suppresses_remint_only_within_same_molecule)
- A blocked clarify bead carrying `finding:<hash>` dedups the same
  clarify-route finding while it remains live, so unresolved
  `loom:clarify` decisions do not remint endlessly
  [test](blocked_clarify_bead_dedups_same_finding_hash)
- `[[suppress]]` entries in `loom.toml` require `reason` and exactly
  one of `id` (canonical finding id) or `hash` (compact finding hash);
  matching rubric-origin findings are reported as suppressed and
  removed from verdict / mint processing
  [test](loom_toml_suppress_entries_filter_rubric_findings_by_id_or_hash)
- Suppressions do not apply to deterministic or integrity findings;
  matching `[[suppress]]` ids or hashes for those findings are
  reported as ineffective and the findings still fail / mint normally
  [test](suppressions_do_not_filter_deterministic_or_integrity_findings)
- A well-formed `LOOM_CONCERN` walk whose every streamed finding is
  suppressed exits clean after emitting suppressed-status records;
  suppression does not forgive malformed stream / terminator pairing
  [test](all_suppressed_concern_walk_exits_clean_after_shape_validation)
- Inline code-comment suppressions are unsupported; the gate does not
  scan source comments for suppression directives
  [check](cargo run -p loom-walk -- no_inline_suppression_comment_contract)
- `LOOM_FINDING_STATUS:` driver-emitted JSON lines carry each enriched
  finding's id, hash, bd label, token, target, and action without
  requiring the LLM to emit derived identity fields
  [test](driver_emits_finding_status_json_with_identity_and_action)
- At `--tree` scope, a live remediation bead whose `finding:<hash>` labels
  are all absent from the current unsuppressed finding set is reported
  as a stale candidate, not auto-closed
  [test](mint_tree_reports_stale_candidates_without_closing)
- At `--tree` scope, a live batch whose finding labels are partially
  stale is reported as a partially-stale candidate with current and
  absent finding ids, not auto-superseded
  [test](mint_tree_reports_partially_stale_batches_without_superseding)
- `-m/--molecule` promotion does not report stale candidates because
  molecule promotion consumes already-recorded deferred findings and
  cannot prove a missing finding is absent from the whole tree
  [test?](mint_molecule_promotion_does_not_report_stale_candidates)
- Each minted batch bead is parented under the scope-selected work epic
  (the molecule work epic for `-m/--molecule`, or the single standing
  remediation work epic for `--tree`) and carries one `finding:<hash>`
  label per contained finding plus one `spec:<X>` label per unique entry
  across the union of `bonds` over the batch's findings
  [test](mint_batches_parent_under_scope_selected_work_epic_with_union_spec_labels)
- The bonding lead is the first element of each finding's `bonds` array
  after validating unique spec epics for bonded specs. Findings sharing a
  lead may bundle into the same lead-spec remediation batch, but the lead
  affects grouping/labels only and does not create a per-spec parent epic
  for tree-scope minting; finding ids and hashes remain unchanged
  [test](mint_bonding_lead_groups_findings_without_selecting_tree_parent_epic)
- For target variants that carry a spec field (currently
  `Criterion` and `Invariant`), `target.spec` MUST appear in that
  finding's `bonds`; a finding that violates this is rejected with
  a typed parse error and the containing mint run is refused
  [test](mint_rejects_criterion_target_whose_spec_is_not_in_bonds)
- Clarify-bound findings mint as single-finding beads (one bead
  per clarify-route finding, not bundled into the spec's remediation
  batch) carrying `finding:<hash>` and `loom:clarify` labels, with
  the description embedding the `## Options — …` block extracted from
  the finding's `evidence` per the *Options Format Contract*
  [test](mint_clarify_bound_finding_creates_single_bead_with_finding_hash_label_and_options_block)
- Any clarify-route finding whose `evidence` lacks a well-formed
  `## Options — <summary>` heading with at least one `### Option
  <N> — <title>` subsection falls back to a remediation bead carrying
  `loom:blocked` with cause `clarify-without-options` — never a
  stranded clarify bead the chat-drafter cannot resolve
  [test](mint_clarify_bound_finding_without_options_falls_back_to_blocked)
- Fix-up batches enumerate every finding in the bead description
  (one item per finding: finding id, hash, token, target's canonical
  form, evidence excerpt); the title is stable across runs for the
  same contained finding-hash set
  [test](mint_batch_description_enumerates_finding_identity_and_title_is_stable)
- A remediation batch carrying multiple findings exposes worker
  discretion to fix all and close, fix a subset and split the remainder
  into sibling remediation beads under the work epic via
  `bd create --parent=<work-epic-id>` for deferred work, or emit
  `LOOM_CLARIFY` for no-progress cases; the bead's acceptance criterion
  is "agent processed the batch", not "every finding individually
  resolved"
  [judge](../tests/judges/loom.sh#judge_remediation_batch_acceptance)
- The per-bead hot path runs deterministic
  `loom gate verify --diff <pre-integration-head>..HEAD` after
  integration and does not invoke focused LLM review or `mint` by
  default
  [test](exec_per_bead_gate_invokes_post_integration_verify_only)
- `mint --tree` walks both the deterministic verifiers and the LLM
  rubric, normalizing findings from either source into the same typed
  mint flow
  [test](mint_tree_scope_walks_verifiers_and_rubric_emitting_findings_from_both)
- Mint is idempotent against partial failure: a crash mid-run leaves
  successfully-minted findings with their `finding:<hash>` labels; a
  re-run's per-finding dedup query skips those hashes and retries only
  the unfinished findings
  [test](mint_idempotent_after_partial_failure_retries_only_unfinished_findings)
- `mint --tree` never leaves an open active remediation work epic with
  zero child beads: if the epic was created but no child bead was
  created before failure, the driver closes or neutralizes the epic and
  restores the `loom:active` bookmark to its pre-run state; if at least
  one child bead was created, the non-empty epic remains open/active and
  rerun dedups the completed children
  [test](mint_tree_partial_failure_never_leaves_empty_active_epic)
- `mint --dry-run` prints proposed bd writes and makes zero bd writes;
  at `--tree` it still walks the rubric/verifiers, and at
  `-m/--molecule` it previews deferred-promotion changes
  [test](mint_dry_run_makes_no_bd_writes)
- `mint` rejects `--spec`; lead-spec routing comes only from each
  finding's typed `bonds` and target after multi-spec lead selection
  [test?](mint_rejects_spec_filter)
- Bare `loom gate mint` prints subcommand help and runs nothing; callers
  choose `--tree` or `-m/--molecule` explicitly
  [test?](mint_bare_invocation_prints_help_and_runs_nothing)
- The end-of-run summary lists blocking findings, deferred findings
  merged, deferred beads promoted, ready remediation batches created or
  updated, clarify findings raised, skipped live finding hashes,
  suppressed rubric findings, stale candidates, partially-stale
  candidates, refused structural conflicts, and transient errors, with
  `LOOM_FINDING_STATUS:` JSON carrying per-finding details
  [test?](mint_end_of_run_summary_reports_finding_lifecycle_outcomes)
- Push-gate integrity findings recover via deferred remediation until
  the molecule's iteration counter exhausts: the verdict gate
  normalizes `UnresolvedAnnotation`, `StubTestFunction`, and
  `UnneededPendingMarker` into typed `Finding`s, merges them into the
  molecule's deferred remediation set, promotes them with
  `loom gate mint -m/--molecule`, refuses the push, increments the
  counter, and re-enters the loop. On cap exhaustion, the gate falls
  back to terminal `loom:clarify` on the molecule's epic with one
  composed auto-generated `## Options — …` block (kind-grouped
  resolutions per *Integrity gate* above)
  [test](push_gate_recovers_integrity_findings_until_cap_then_clarifies)
- The bead-container worker runs the injected exact self-check range
  (`loom gate verify --diff <bead-base>..HEAD`, or `@{u}..HEAD` only
  when upstream is that base) before emitting `LOOM_COMPLETE`, reruns it
  after any later commit or hook-generated change, and performs
  prompt-level self-review before final marker emit
  [judge](../tests/judges/loom.sh#judge_loop_preflight_exact_range_and_self_review)

### Gate evidence and marker coverage

- Every `loom gate` invocation that runs work writes a JSONL gate log
  under `.loom/logs/gate/` and emits `driver_event` records for
  `gate_run_start`, `gate_run_scope`, per-lane progress, and
  `gate_run_end`; a start without an end is treated as incomplete
  evidence, not success
  [test](gate_invocations_emit_jsonl_lifecycle_events)
- `DriverKind` is a typed enum with an `Other(String)` fallback; gate
  lifecycle kinds (`GateRunStart`, `GateRunScope`, `GateRunLane`,
  `GateRunEnd`, `GateRunSkipped`) serialize through the existing
  `driver_event.driver_kind` string field rather than new top-level
  `AgentEvent` variants
  [test](driver_kind_typed_enum_carries_gate_lifecycle_values)
- `VerifiedScope` is constructible only from a successful deterministic
  `GateRun`; `ReviewedScope` is constructible only from a successful
  review run; `GateSuccess` is constructible only from matching
  `VerifiedScope`, `ReviewedScope`, pre-push hook coverage, and current
  tree/config/range fingerprints
  [test?](gate_success_requires_matching_typed_scope_evidence)
- `loom gate review --diff <range>` consumes the latest matching
  `VerifiedScope` for the same resolved content/scope when producing
  push-eligible evidence; no production gate path passes or accepts
  `--verify-exit`
  [test?](gate_review_consumes_verified_scope_not_verify_exit_scalar)
- A driver-minted marker short-circuits a wrapped pre-push hook only
  when it proves the same tree OID / clean porcelain, same
  `.pre-commit-config.yaml` digest, same resolved push range, a
  successful pre-push `GateRun` containing that hook id/entry as
  passed, and successful `VerifiedScope` + `ReviewedScope` for the
  same range; otherwise `pre-push-checks` falls through
  [test?](marker_short_circuit_requires_hook_coverage_for_same_tree_config_and_range)

### Wire-format wiring and dead-code excision

The production wiring obligation — every caller that constructs
`GateInputs` for the review-phase verdict gate must populate
`streamed_findings` from a real `parse_walk_output` invocation
rather than leaving it at default — is owned by
[harness.md § Verdict gate](harness.md#verdict-gate). This
subsection covers the wire-format-side dead-code excision and the
ReviewConcern display-vocabulary retirement that the wiring change
depends on.

- `ReviewError::ConcernWithoutBeadDeltas` is removed from
  `crates/loom-workflow/src/review/error.rs` and the raise site at
  `review/runner.rs` is removed in the same diff; no production code
  path constructs this error variant
  [test](no_path_constructs_concern_without_bead_deltas_in_production)
- `parse_review_flag` (the legacy `<token> -- <reason>` whole-stdout
  hunter) is removed from `crates/loom-workflow/src/review/phase_verdict.rs`;
  the function and all its callers are deleted
  [test](parse_review_flag_is_not_defined_or_called_in_production)
- `decide_concern` no longer parses the terminal `summary` field as a
  `ReviewConcern` token; the legacy fallback path
  (`ReviewConcern::parse(summary)`) is removed. An unrecognized
  summary with at least one streamed finding routes to
  `RecoveryCause::ReviewConcern { summary, findings }`, not
  `SwallowedMarker`
  [test](decide_concern_unrecognized_summary_with_findings_routes_to_review_concern_not_swallowed)
- The `ReviewConcern` 12-variant enum stays as a display vocabulary
  for `bd update --notes` and verdict-log human-readable cause
  labels, but `ReviewConcern::parse` has no production caller. The
  per-finding render in `PreviousFailure::ReviewConcern` derives the
  human-readable label from `findings[0].token` (or a "multiple"
  label when heterogeneous)
  [test](previous_failure_review_concern_renders_human_label_from_findings_not_summary)

### Wire-format strict validation and max-context preservation

- A `LOOM_FINDING:` record whose JSON payload fails parse after raw
  string line-break normalization — invalid JSON (most common: trailing
  backticks from markdown fencing), unknown `token`, target/token
  variant mismatch, unresolved spec label or anchor — surfaces as
  `RecoveryCause::BadWalk(BadWalk::MalformedFinding { errors, terminal })`
  with the well-formed terminal preserved alongside the per-record
  parse errors
  [test](backtick_wrapped_loom_finding_line_routes_to_bad_walk_malformed_finding_with_terminal_preserved)
- The `LOOM_FINDING:` substring match is case-sensitive and
  colon-suffixed; bare-prose mentions without the colon do not match
  [test](loom_finding_substring_match_requires_uppercase_and_colon_suffix)
- `BadWalk::Concern` carries `{ payload, parsed_findings: Vec<Finding> }`;
  well-formed findings streamed ahead of a malformed terminal are
  preserved in `parsed_findings`
  [test](bad_walk_concern_preserves_well_formed_findings_alongside_malformed_payload)
- `BadWalk::FindingsWithoutConcern` carries
  `{ finding_count, findings: Vec<Finding> }`; the parsed findings
  ride through so the next iteration's prompt and `loom gate mint`
  can both consume them
  [test](bad_walk_findings_without_concern_carries_parsed_findings_vec)
- The `BadWalk::MalformedFinding { errors, terminal }` variant
  carries every per-record parse error AND the well-formed terminal
  surface (or a typed `Missing`/`Malformed` variant when the
  terminal itself failed). Construction without both pieces is a
  compile error
  [test](bad_walk_malformed_finding_variant_carries_errors_and_terminal_by_struct_shape)

### Verification surface (matrix + property)

- The review-phase classifier signature consumes a typed `WalkOutput`
  (with field-private struct + `pub WalkOutput::from_stdout`
  constructor that runs `parse_walk_output` internally), not raw
  `&str`. Any production caller passing a `&str` is a compile error,
  and any caller constructing `WalkOutput` with bogus fields cannot
  compile because the fields are private at the `loom-protocol`
  crate boundary
  [test](classify_review_phase_signature_requires_typed_walk_output)
- The (stream-shape × terminal-shape) failure matrix is exhaustive:
  every cell in the 4 × 6 cross-product (S0..S3 stream shapes × six
  terminal shapes) has a parameterised test asserting the typed
  outcome variant and the maximum-context invariant
  [test](walk_output_failure_matrix_routes_every_cell_with_typed_outcome_and_preserves_max_context)
- Every constructible `Finding` (each `ConcernToken` × canonical
  `FindingTarget` combination) round-trips byte-equal through
  `serde_json::to_string` → embed in a `LOOM_FINDING:` record →
  embed in a synthetic walk output → `parse_walk_output`, with
  stable finding id and hash
  [test](every_finding_round_trips_through_wire_format_with_stable_identity)
- `ConcernToken::CrossSpecClash` round-trips through the wire format
  with canonical target `Criterion { spec, anchor }` and is exercised
  by the round-trip property test cell set
  [test](concern_token_cross_spec_clash_round_trips_with_criterion_target)
- `ConcernToken::SpecConventionsViolation` round-trips through the
  wire format with canonical target `Criterion { spec, anchor }` and
  is exercised by the round-trip property test cell set
  [test](concern_token_spec_conventions_violation_round_trips_with_criterion_target)
- `cross-spec-clash` and `spec-conventions-violation` are
  tree-scope-only tokens: the rubric emits them at `--tree` scope;
  `--diff` / `--files` scope rejects them. A finding carrying either
  token parsed at non-tree scope surfaces a typed `FindingParseError`
  variant naming the scope mismatch, alongside the existing
  diff-context restriction on `scope-creep` / `scope-shortfall`
  [test](tree_scope_only_tokens_rejected_at_non_tree_scope)

### `loom-protocol` crate

The Rust contract for the gate's wire format lives in
`loom-protocol::gate` — a leaf crate carrying the typed `Finding`
record + closed-set `ConcernToken` enum + `FindingTarget` /
`TargetKind` / `FindingValidator` / `FindingParseError` + `BadWalk` /
`TerminalSurface` / `WalkOutput` / `WalkOutputError` / `ExitSignal` +
the `parse_walk_output` / `WalkOutput::from_stdout` /
`parse_exit_signal` parsers + the `LOOM_FINDING_PREFIX` constant. Per
*Canonical contract location*.

- The `loom-protocol` crate exists as a leaf workspace member with
  the `gate` module carrying every type listed above. The crate's
  dependencies are limited to `serde`, `serde_json`, `thiserror` /
  `displaydoc`, `blake3` (finding-hash crate — algorithm is
  implementer's choice per *Finding id, finding hash, suppression,
  and dedup*, but the dep set is closed), and `loom-events` (for
  `SpecLabel`); no transitive
  dependency on `loom-templates`, `loom-workflow`, or `loom-gate`
  [test](loom_protocol_crate_has_minimal_leaf_dependency_set)
- `loom-templates::finding` and `loom-templates::previous_failure` re-
  export the typed contract from `loom-protocol::gate` via `pub use`
  so existing callers (including `PreviousFailure::ReviewConcern {
  findings }`) compile without changes. The original definitions are
  removed from `loom-templates`
  [test](loom_templates_re_exports_finding_contract_from_loom_protocol)
- `loom-workflow::review::finding` (the `WalkOutput` typed product +
  `parse_walk_output` parser) and `loom-workflow::todo::exit::ExitSignal`
  / `parse_exit_signal` move to `loom-protocol::gate`. Existing
  `loom-workflow` imports either remap or re-export
  [test](loom_workflow_re_exports_walk_output_and_exit_signal_from_loom_protocol)
- The `WalkOutput` struct's fields are private; `WalkOutput::from_stdout`
  is `pub` (consumers need to call it) but is the only construction
  path. The silent-loss failure class — production caller constructs
  `WalkOutput` with bogus fields, bypassing the typed parse pipeline —
  is structurally unrepresentable via field-privacy, not via
  `pub(crate)` constructor scoping
  [test](walk_output_fields_private_only_constructor_is_from_stdout)
- The crate's MAJOR version is the wire-format protocol version. A
  breaking wire change (renamed token, retyped target shape, removed
  enum variant) requires a major bump. Additive changes (new
  `ConcernToken` variant, new `FindingTarget` variant, new fields with
  `#[serde(default)]`) are minor bumps. No `"protocol": <n>` field
  appears on `LOOM_FINDING:` / `LOOM_CONCERN:` payloads — the typed
  parse errors give loud structural breakage on version skew
  [test](loom_protocol_wire_format_does_not_carry_protocol_version_field)
- The `finding_no_duplicate_definitions` walker continues to enforce
  one canonical definition of `Finding`, `ConcernToken`,
  `FindingTarget`, `WalkOutput`, `BadWalk`, and `ExitSignal` across
  the workspace; the canonical home after extraction is
  `loom-protocol::gate`
  [check](cargo run -p loom-walk -- finding_no_duplicate_definitions)

### Production walker wiring

The seam between `loom gate mint --tree`'s CLI arm and the underlying
walker is the `MintWalker` trait. The tree-sweep CLI arm dispatches
through the trait so findings reach the mint pipeline from a real walk.
A tree-sweep arm that constructs an empty finding vector
unconditionally — bypassing the walker — is a structural defect. The
`-m/--molecule` arm is different: it promotes already-recorded
deferred bd findings and does not fabricate a new `Vec<Finding>`.

- A production `MintWalker` implementation exists in
  `loom-workflow::mint::walk` (alongside the trait). Its `run_rubric`
  spawns the reviewer agent subprocess against the rendered review
  prompt and returns the agent's combined stdout; its `run_verifiers`
  dispatches the deterministic verifier set + the integrity gate
  forward-resolution check and returns one `VerifierFailure` per failed
  dispatch outcome. Both methods are used only for `MintScope::Tree`
  [test](production_mint_walker_exists_and_dispatches_rubric_and_verifiers)
- `run_gate_mint` in the loom CLI binary dispatches by scope:
  `--tree` constructs production walker(s) and obtains findings only
  through the `MintWalker` trait (`run_verifiers` once for the tree,
  then `run_rubric` per walked spec) before passing collected findings
  to the minting pipeline. A verifier/rubric source failure records an
  error in the mint summary and exits non-zero, but does not discard
  findings already collected from other specs; stale-candidate reporting
  is suppressed for that incomplete tree walk. `-m/--molecule` calls the
  deferred-promotion path and never constructs a placeholder empty
  findings vector
  [test](run_gate_mint_dispatches_tree_through_walker_and_molecule_through_promotion)
- `loom loop`'s per-bead path routes a loop-phase `Success` outcome
  through exactly one post-integration deterministic gate result; a
  clean result makes the bead's integration durable (neither clarified
  nor blocked). The bullet below pins the subprocess shape that the
  per-bead gate resolves to; deferred remediation beads are not made
  ready until the molecule stabilization step promotes them
  [test?](loop_per_bead_routes_run_phase_success_through_post_integration_gate)
- The production per-bead gate implementation spawns exactly one
  deterministic subprocess against `loom_bin` after integration — argv
  shape `gate verify --diff <pre-integration-head>..HEAD`. A regression
  that invokes `gate mint`, passes `--bead` / `--spec`, invokes
  `--verify-exit`, or starts a focused review session on the per-bead
  hot path is caught at the production controller
  [test](exec_per_bead_gate_invokes_post_integration_verify_only)

### Molecule mint summary semantics

- `loom gate mint -m/--molecule <id>` exits 0 when it successfully
  promotes zero or more deferred remediation beads; the summary lists
  promoted counts and any reobserved closed findings
  [test](mint_molecule_exits_zero_on_successful_promotion_summary)
- `loom gate mint -m/--molecule <id>` exits non-zero when promotion
  sees a structural conflict (duplicate live finding hashes, missing
  work epic, or bd write failure), and the summary names the
  conflicting bead ids or bd error
  [test](mint_molecule_exits_nonzero_on_structural_or_write_errors)

Loop-side interpretation of these exit codes — retrying transient
promotion errors or blocking on structural bd state — is owned by
[harness.md § Functional](harness.md#functional).

### Commands surface — explicit scopes and status

- Bare `loom gate` (no subcommand) prints `loom gate --help` —
  identical output to `loom gate --help`. No verifier runs, no cache
  read, no bd writes
  [test](bare_loom_gate_prints_subcommand_help)
- Bare `loom gate verify` prints the verify subcommand help and runs no
  verifier, project hook, cache lookup, bd write, or marker check
  [test](bare_loom_gate_verify_prints_help_and_runs_nothing)
- `loom gate verify` rejects a positional selector; exact target
  selection uses `--target <annotation target>`
  [test](gate_verify_rejects_positional_selector)
- `--target` is mutually exclusive with `--files`, `--diff`, and
  `--tree`; zero matches fail loudly; cross-tier matches under
  `verify --target` fail and suggest a tier subcommand; multiple
  criteria sharing the same target inside one tier are accepted
  [test](gate_target_exact_match_and_ambiguity_rules)
- `loom gate verify --diff <base>..<head>` runs the project pre-commit
  lane via prek for the resolved range before the spec annotation lane;
  if a hook modifies the working tree the run exits non-zero with a
  tree-modified-by-hook failure
  [test](verify_diff_runs_prek_pre_commit_lane_before_annotations)
- A nested `loom gate verify --files` invoked under a parent
  `verify --diff` records a skipped gate event with reason
  `parent-diff-gate` and exits 0; correctness does not depend on the
  project's hook id
  [test](nested_verify_files_under_parent_diff_gate_records_skip)
- Scope-derived tier policy has no `LOOM_VERIFY_TIERS` override:
  `verify --files` runs affected `[check]`/`[test]`, `verify --diff`
  runs project pre-commit plus affected `[check]`/`[test]`, and
  `verify --tree` runs full `[check]`/`[test]`/`[system]`
  [test](verify_tier_policy_is_scope_derived_without_env_override)
- `loom gate status --diff <range>` / `--files <paths...>` / `--tree`
  reads criterion evidence from `.loom/cache.db` and prints the report
  per `Status cache` above; status without an explicit scope prints help
  and runs no cache lookup
  [test](loom_gate_status_requires_explicit_scope)
- `loom gate status` is `refused_inside_loom() == false`; running
  under `LOOM_INSIDE=1` is allowed because the cache read is local
  and read-only
  [test](loom_gate_status_is_allowed_under_loom_inside_env)

### Status cache

- `.loom/cache.db` is created on first `open` when the path is missing
  [test](open_creates_db_file_when_missing)
- A criterion-evidence `CacheRow` round-trips through sqlite preserving
  every field, including typed `(SpecLabel, CriterionId)` identity and
  the current annotation snapshot
  [test](round_trip_through_sqlite_preserves_every_field)
- The `row_for` helper writes a row that round-trips through the unified
  cache
  [test](row_for_helper_writes_round_trip_row)
- Report rendered from on-disk rows summarises pass/fail per tier
  [test](render_report_reads_from_disk_and_summarises_per_tier)
- Broken-annotation entries in the report come from integrity findings,
  not from the cache file itself
  [test](broken_annotations_in_report_come_from_integrity_findings)
- **Cache render <500ms — sqlite path.** The report renders in <500ms
  on a 2000-row corpus when read from sqlite (hard target from
  *Status cache*)
  [test](render_under_500ms_on_2000_row_corpus)
- **Cache render <500ms — in-memory path.** Same <500ms target holds
  for the in-memory `render_from_rows` path
  [test](render_from_rows_under_500ms_on_2000_row_corpus)

### Verifier inputs

- `[test]` annotations resolve declared inputs as the union of the
  owning crate's source directories (via `cargo metadata`) and the spec
  section the annotation lives in (spec-section auto-include)
  [test](test_tier_resolution_uses_cargo_metadata_plus_spec_autoinclude)
- A `[judge]` target is located by shared selector-stripping (`#fn` /
  `::fn` / `::attr`) plus spec-relative resolution, the same helper the
  integrity gate uses, so the input resolver and integrity gate cannot
  disagree about where the script lives
  [test](judge_tier_strips_selector_and_collects_relative_to_spec_dir)
- A judge script reports per-function inputs via `<script> --print-inputs
  <fn>` collect mode — `judge_files` records its path arguments while
  `judge_criterion` and the LLM call are skipped, and the recorded paths
  are emitted as `{"inputs":[...]}`
  [test](judge_collect_mode_records_judge_files_paths)
- The input-query batch form maps each target to its globs in one spawn —
  `<script> --print-inputs` with no `<fn>` emits `{"inputs":{"<fn>":[...]}}`
  for every rubric, so discovery spawns no more processes than batched
  execution
  [test](batch_print_inputs_maps_each_target_to_its_globs)
- The `--print-inputs` query is issued through the verifier's command
  template, not by prepending the flag to the command's first token, so a
  `cargo run -p <crate> -- <walk>` verifier is queried as the walk's own
  argument
  [test](print_inputs_issued_through_command_template_not_argv_head)
- A verifier that reports no inputs of its own always runs under a
  `--files` scope — the resolver never narrows an undeterminable input
  set to the spec section alone
  [test](undeclared_verifier_always_runs_under_files_scope)
- An opted-in input-query that exits non-zero or emits a malformed
  inputs document is flagged `inputs-protocol-error` — opt-in being a
  `[judge]` collect mode or a `[check]` / `[system]` runner that
  declares an `inputs` query
  [test](opted_in_input_query_failure_flagged_inputs_protocol_error)
- A verifier whose input-query contract loom does not own — an
  unregistered command, or a runner with no `inputs` query — falls
  through to the conservative always-run default without an
  `inputs-protocol-error`
  [test](unowned_verifier_input_query_falls_through_silently)
- A `[check]` / `[system]` target that matches a runner resolves via
  that runner, not via a `tokens[0]` PATH/file check; only an unmatched
  target falls back to the `tokens[0]` check
  [test](runner_matched_target_resolves_via_runner_not_token_path_check)
- A runner-matched verifier's queried inputs decide its `--files` scope
  inclusion: the scope filter keeps the matched sibling whose queried
  glob is staged and drops the one whose glob is disjoint, priming the
  matched `[check]` group in a single query spawn
  [test](filter_by_files_keeps_runner_matched_check_sibling_whose_queried_input_is_staged)

### Scope handling

- Live-workspace scope for a `[test](crate::module::test)` annotation
  includes the owning crate's files plus its transitive dependency
  files
  [test](live_workspace_scope_includes_own_files_and_transitive_dep_files)
- Live-workspace scope for an annotation referencing an unknown crate
  is empty
  [test](live_workspace_scope_for_unknown_crate_is_empty)
- Live-workspace scope for a `[test](<crate>)` placeholder-target
  annotation is empty
  [test](live_workspace_scope_for_crate_placeholder_target_is_empty)

### Dispatch — per-tier process model

- Runner-matched `[check]` annotations batch into one subprocess per
  runner
  [test](run_check_batches_loom_walk_shaped_targets_through_one_runner_spawn)
- Unmatched `[check]` annotations use the fallback path and spawn one
  subprocess per annotation
  [test](dispatcher_spawns_one_subprocess_per_unmatched_check_annotation)
- `[system]` tier spawns one subprocess per annotation (system
  verifiers are inherently slow and self-contained; batching doesn't
  help)
  [test](dispatcher_spawns_one_subprocess_per_system_annotation)
- `[test]` tier batches every in-scope target into one runner
  subprocess per invocation
  [test](test_tier_batches_all_targets_into_one_runner_subprocess)
- `[test]` tier filters targets by `--files` scope intersection before
  invoking the runner
  [test](test_tier_filters_targets_by_files_scope_intersection)
- `[test]` tier returns no subprocess when the `--files` filter
  excludes every target
  [test](test_tier_returns_none_when_files_filter_excludes_everything)
- `[test]` tier returns no subprocess when no `[test]` annotations are
  in scope at all
  [test](test_tier_returns_none_when_no_test_annotations_in_input)
- `[judge]` tier batches every target into one runner subprocess per
  invocation
  [test](judge_tier_batches_all_targets_into_one_runner_subprocess)
- `[judge]` tier ignores `--files` scope filtering (unlike `[test]`)
  [test](judge_tier_ignores_files_scope_unlike_test_tier)
- Dispatcher skips annotations whose tier does not match the requested
  tier
  [test](check_tier_skips_annotations_with_non_check_tier)

### Dispatch — env contract

- The dispatcher sets `LOOM_FILES` and `LOOM_SPEC` env vars on every
  verifier subprocess (per *Verifier-runner contract*)
  [test](dispatcher_sets_loom_files_and_loom_spec_env_on_verifier_subprocess)

### Dispatch — JSON verdict and exit-code fallback

- `[check]` tier falls back to "exit code 0 → pass" when the verifier
  emits no JSON line (per *Fallback for non-conforming verifiers*)
  [test](check_tier_falls_back_to_exit_code_pass_when_verifier_omits_json)
- `[check]` tier falls back to "non-zero exit → fail" when the verifier
  emits no JSON line
  [test](check_tier_falls_back_to_exit_code_fail_when_verifier_omits_json)
- `[test]` runner falls back to exit code when the runner omits a JSON
  per-target line
  [test](test_tier_falls_back_to_exit_code_when_runner_omits_json_line)
- A malformed JSON verdict (e.g. `pass` field with wrong type) surfaces
  as a typed dispatch error rather than silently passing
  [test](dispatcher_surfaces_malformed_verdict_when_pass_key_has_wrong_type)
- Incidental JSON on stdout that isn't a recognised verdict line falls
  through to the exit-code path
  [test](dispatcher_falls_through_to_exit_code_on_incidental_json)
- A verifier command that fails to spawn (command not found) surfaces
  as a dispatch error — the gate-exit-2 case from the
  *Verifier-runner contract*
  [test](dispatcher_surfaces_spawn_failure_when_command_not_found)

### Runners — batched dispatch

- `run_with_runners` groups matched annotations into one batch per
  runner and falls back to per-annotation spawn for unmatched
  annotations
  [test](run_with_runners_groups_matched_into_one_batch_and_falls_back_for_unmatched)
- When multiple runners' `match` regexes could apply, the first match
  in spec order wins
  [test](run_with_runners_first_match_wins_in_spec_order)
- When a batched-runner invocation does not produce per-target output
  for every annotation in the batch, the missing targets surface as
  dispatch failures
  [test](run_with_runners_dispatch_fails_targets_missing_from_batch_output)
- Runner cwd resolution — explicit `cwd` is resolved against the repo
  root
  [test](run_with_runners_resolves_cwd_against_repo_root)
- Runner cwd resolution — a runner with no `cwd` falls through to the
  tier-default `cwd`
  [test](run_with_runners_falls_through_to_tier_default_when_runner_cwd_is_none)
- Runner cwd resolution — a runner with no `cwd` and no tier-default
  uses the repo root
  [test](run_with_runners_uses_repo_root_when_neither_runner_nor_tier_cwd_set)
- Tier-default `cwd` also applies to per-annotation fallback when the
  matched runner has no cwd
  [test](run_with_runners_tier_default_applies_to_unmatched_per_annotation_fallback)
- `libtest-json` parser maps test-event names back to annotation
  targets
  [test](run_with_runners_libtest_json_maps_test_names_back_to_annotations)
- `exit-code` parser shares a single per-runner verdict across every
  target in the group
  [test](run_with_runners_exit_code_parser_shares_verdict_across_group)

## Not in scope of this spec

The gate enforces; it does not own:

- The *content* of the consumer's style-rules document — which rules
  exist, how they're organised, what prefixes the consumer uses. The
  gate references the rules; the rules are authored by each consumer.
- The *content* of `[check]` / `[test]` / `[system]` / `[judge]`
  verifiers. The gate runs them; they live in the consumer's repo.
- The *organisation* of the consumer's verifiers — whether the
  `[check]`-tier walks live in a dedicated crate, are scattered
  across source crates, or are shell scripts is the consumer's
  choice. The gate dispatches whatever annotation says, however the
  consumer chooses to back it.
- Workflow events (push, merge, bead lifecycle, remediation bonding,
  molecule progress). Those are downstream of the gate's verdict, not
  properties the gate evaluates.
- The `loom:clarify` resolution channel itself — `loom inbox` is the
  surface, defined in [harness.md § Inbox Modes](harness.md#inbox-modes).

# Loom Gate

The quality gate. Decides whether code is good enough to ship.

The umbrella concept covering all four stages (plan / per-diff /
push / standing safety net) and one command tree (`loom gate
<subcommand>`).
`loom gate verify` runs deterministic verifiers; `loom gate review`
runs the LLM rubric (inspection-only); `loom gate mint` walks the
rubric and produces fix-up beads — the only subcommand that
mutates bd state. Distinct from the *Verdict Gate* execution layer
in [harness.md](harness.md) — that section owns the per-bead
mechanics that wrap the gate; this spec owns the rubric, the
invariants, the lanes, and the stages.

## Problem Statement

Loom's review machinery has multiple participants: a verdict gate
in [harness.md](harness.md) (per-bead, per-diff,
narrowly scoped), style rules in
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
   whether any merge is in flight.** Per-diff review can only see
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
  recovery (recovery loop at per-diff, push refused at push, fix-up
  bead at standing) drives the response, all converging on *fix the
  code*.

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
  below) and waits for `loom msg` resolution.

## Commands

The gate is one umbrella command, `loom gate`, with subcommands
selecting the audit scope:

| Command | Kind | Purpose |
|---|---|---|
| **`loom gate`** | Help | Prints `loom gate --help` — the subcommand list with one-line descriptions. No verifiers run, no cache read. Discoverability surface for operators approaching the gate; the fast cached-status path is `loom gate status`. |
| **`loom gate status`** | Status | Reads cached results from the last `verify` / `review` / `audit` run (sqlite-backed) and prints a fast status report — no verifiers run. Inherits the bare-invocation scope default (active molecule's diff if the active spec has an open epic, else `HEAD`), so `loom gate status` answers "where does my current molecule stand?" by default; `loom gate status --tree` for the full workspace. See *Status cache* for the hard latency target. |
| **`loom gate audit`** | All, inspection | Runs everything — `verify` then `review`. Inspection composition (no bd writes); the push gate runs this for its verdict, and operators run it for ad-hoc review. The act path is `mint`. |
| **`loom gate verify`** | Deterministic | Runs every `[check]` / `[test]` / `[system]` verifier. Cheap relative to review; expensive relative to status. Run frequently (pre-commit, on save, every CI commit). |
| **`loom gate check`** | Deterministic, one tier | Runs only `[check]`-tier verifiers (static analysis of source). Fastest tier; suitable for tight feedback loops. |
| **`loom gate test`** | Deterministic, one tier | Runs only `[test]`-tier verifiers, batched into one runner subprocess per invocation. |
| **`loom gate system`** | Deterministic, one tier | Runs only `[system]`-tier verifiers (containers, packaging, end-to-end). Slow; CI-only by default. |
| **`loom gate review`** | LLM judge, inspection | Runs the LLM rubric — conformance trace, contract closure, verifier honesty, mock discipline, invariant-clash scan. **Inspection-only**: emits `LOOM_FINDING:` lines + terminal marker to stdout, makes no bd writes. Run selectively (bead completion, on demand, scheduled). Consumes `verify` results as input. Includes `[judge]`-tier criterion verifiers and the rubric walk over the diff. |
| **`loom gate judge`** | LLM judge, one lane | Runs only criterion-attached `[judge]` verifiers — skips the rubric walk. Inspection-only, like `review`. |
| **`loom gate rubric`** | LLM judge, one lane | Runs only the rubric walk over the diff — skips criterion-attached judges. Inspection-only, like `review`. |
| **`loom gate mint`** | Act | Walks the rubric (per-`--bead`/`--diff`/`--files` scope: LLM rubric only; per-`--tree` scope: deterministic verifiers + LLM rubric) and mints fix-up beads from typed findings, bonded per-spec via the molecule lifecycle. The sole driver-side mint chokepoint and the actor in `loom loop`'s per-bead path. See [*Findings and Minting*](#findings-and-minting). |
| **`loom gate verify-marker`** | Trust check | Reads `.loom/marker.json` from the current workspace, deserializes a typed `MarkerProof`, computes the current workspace fingerprint (tree OID at HEAD + porcelain-clean precondition), and exits 0 iff the marker validates against the current fingerprint. Used by prek's pre-push hook chain to short-circuit redundant work on driver-loop integration pushes. See [*Marker*](#marker). |

### Scope flags

All gate subcommands take exactly one scope flag (mutually
exclusive), plus optional filters. The scope flag defines the
**input set** — the files the gate is being asked about. A verifier
runs iff its declared inputs intersect the input set (see *Verifier
inputs* below); otherwise it's skipped.

| Flag | Input set | Typical caller |
|---|---|---|
| `--bead <id>` | The bead's success-criteria input files + the bead's own diff | `loom loop` per-bead verdict gate |
| `--diff <range>` | `git diff <range> --name-only` (committed + working tree in the range) | push gate (molecule.base_commit..HEAD); CI scoped to a PR; bare interactive `loom gate verify` |
| `--files <paths>` | Explicit path list | pre-commit hooks (`loom gate verify --files $(git diff --cached --name-only)`) |
| `--tree` | Every file in the workspace | nightly CI safety net; manual debugging; **not used by push gate** |

Filters compose with any scope flag:

- `--spec <label>` — narrow to one spec's criteria
- `<selector>` (positional) — run one specific verifier by its
  annotation target

**Default for bare invocation.** When a gate subcommand is invoked
without a scope flag, the gate defaults to `--diff
<molecule.base_commit>..HEAD` if the active spec has an open epic
(single-query resolution; the "what would fail if I pushed now?"
question); else `--diff HEAD` (working-tree dirty changes vs HEAD).
This applies uniformly across subcommands that take scope flags
(`status`, `verify`, `audit`, `review`, `mint`, the tier subcommands,
…). Bare `loom gate audit` never silently expands to `--tree` —
users who want the full safety-net sweep type `--tree` explicitly.
(Bare `loom gate` with no subcommand prints subcommand help, per
*Commands* above.)

| Stage | Default invocation | Scope |
|---|---|---|
| Pre-commit hook | `loom gate verify --files $(git diff --cached --name-only)` | `--files` |
| `loom loop` per-bead (verify) | `loom gate verify --bead <id>` | `--bead` |
| `loom loop` per-bead (mint, runs on verify pass) | `loom gate mint --bead <id>` | `--bead` |
| `loom loop` molecule completion (receipt construction) | `loom gate audit --diff <molecule.base_commit>..HEAD` | `--diff` |
| Interactive bare `loom gate verify` / `loom gate status` | implicit `--diff <molecule.base_commit>..HEAD` if active spec has an open epic; else `--diff HEAD` | `--diff` |
| Nightly CI / on-demand inspection | `loom gate audit --tree` | `--tree` |
| Nightly CI / on-demand mint | `loom gate mint --tree` | `--tree` |

**Why push gate isn't `--tree`.** A `--tree` sweep runs every
verifier against every spec; on a non-trivial workspace this takes
many minutes. The push gate doesn't need that — the molecule's
claim is "the work *I* did is done and correct", which is
exclusively about files the molecule changed. Verifiers whose
inputs don't intersect the molecule's diff have results unchanged
from when they last ran; skipping them is safe. `--tree` is the
nightly safety net that catches verifier-input-declaration drift
(see *Verifier inputs*), not the push gate.

The composition: `loom gate audit` ≡ `loom gate verify && loom gate
review`. Both are inspection paths; `audit` produces no bd writes.
For lane subsets without a named alias (e.g. "all deterministic
without `system`"), shell composition is the path:
`loom gate check && loom gate test`. The act path is `loom gate
mint`, which walks and writes; see [*Findings and Minting*](#findings-and-minting).

## Stages

Same gate, four points. Scope and cost-of-failure differ; the
underlying check is the same.

| Stage | Where | Scope | Cost-of-failure | Primary catches |
|---|---|---|---|---|
| **Plan** | `loom plan -n` / `loom plan -u` | Spec under interview | Lowest — no code yet | Missing claims, weak claims, missing verifier surfaces, invariant clashes in proposed spec changes |
| **Per-diff** | In `loom loop`: `loom gate verify --bead <id>` then `loom gate mint --bead <id>`. For ad-hoc inspection: `loom gate audit --bead <id>` (verify + review, no minting) | Spec sections the diff touches; the diff itself; tests in the diff | Medium — one bead's worth | Conformance gaps in diff, lint violations, weak verifiers, contract gaps inside one diff's reach, invariant clashes in proposed code changes |
| **Push** | `loom gate audit --diff <molecule.base_commit>..HEAD` (unconditionally on `loom loop` molecule completion — see [harness.md FR1 + FR9](harness.md#functional)) | The molecule's own diff (files it touched) × every verifier whose declared inputs intersect that diff | Highest — **blocks push**, gate verdict encoded in [`GateOutcome`](harness.md#loop-outcome-types) (`Success`/`Fail`/`NoGate`). `GateSuccess` is constructible only when all four FR9 conditions hold *and* on-disk review-log evidence is present; the type's `pub(crate)` constructor asserts each condition. Silent gate-skip is structurally unrepresentable | Conformance gaps in the molecule, integrity-gate findings (unresolved annotations, stub tests) within the molecule's diff, review concerns, dispatch errors |
| **Standing safety net** | `loom gate audit --tree` for inspection; `loom gate mint --tree` to act (on-demand, nightly CI, scheduled). The mint path is the only one that creates fix-up beads — see [*Findings and Minting*](#findings-and-minting) | Entire spec tree × entire implementation | Catches **verifier-input-declaration drift** — any verifier the push-gate's `--diff` scope would have skipped on the same diff is surfaced here. Findings (including drift) surface as regular fix-up beads via mint; `invariant-clash` findings additionally carry `loom:clarify` for human resolution | Cross-file incoherence the molecule's diff didn't surface, contracts orphaned across PRs, accumulated style/test regressions, template-vs-spec drift (Invariant 3), surface drift, verifier-input declarations that are too narrow |

The plan stage has no separate command invocation — the agent runs
the rubric inline during the planning interview, and `loom plan` is
the surface that opens that interview. The remaining three stages
compose gate subcommands per the table above: per-diff in the
`loom loop` path runs `verify` then `mint`; push runs `audit`
(`verify` + `review`) for inspection only; the standing safety net
triad of `verify` / `review` / `mint` runs at tree scope, with
`mint` the act surface and `verify` / `review` (or composed via
`audit`) the inspection surfaces.

The push stage is **non-optional and load-bearing across every
execution mode of `loom loop`** — sequential, parallel, `--once`,
and `--all-specs`. It computes the four-condition AND of FR9 (bead
labels + verify exit + review exit + integrity findings) and refuses
push on any failure. The verdict is encoded in
[`GateOutcome`](harness.md#loop-outcome-types): `Success` only
when all four conditions hold *and* the review wrote a non-empty
`review-*.jsonl` ending in a terminal `LOOM_COMPLETE` marker; `Fail`
on any failure with the reason explicit; `NoGate` only for legitimate
"no work to gate" terminals (`NoBeadsReady`, `OncePartial`). The
`GateSuccess` constructor is `pub(crate)` in the gate-invocation
module — no code path outside that module can mint one, so a clean
`loom loop` exit without the gate actually firing is unrepresentable.
The standing safety net is **scheduled, not load-bearing for any
individual push** — its job is to catch verifier-input-declaration
drift over time, not to gate per-molecule pushes.

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

### Per-diff stage checks

The per-diff stage in `loom loop` composes `loom gate verify` and
`loom gate mint` in sequence. `loom gate audit --bead <id>` (which
composes `verify` + `review`) is the inspection-only alternative for
ad-hoc human review; the loop never runs `review` directly.

**Production wiring.** `loom gate mint`'s walk is dispatched via the
[`MintWalker`](#scope-dependent-walk) trait (defined in
`loom-workflow::mint::walk`). The trait's `run_rubric` method spawns
the reviewer agent subprocess (same dispatch the per-bead `review`
controller uses, against the rubric prompt rendered by the
`loom-templates::review` context) and returns its raw stdout; its
`run_verifiers` method dispatches the deterministic verifiers and the
integrity gate's forward-resolution check at `--tree` scope only
(per *Scope-dependent walk* below). The CLI arm `run_gate_mint`
constructs the production walker, calls `mint::walk::walk(...)`, and
feeds the resulting `Vec<Finding>` into `mint_findings_with_options`.
The walker is the only path findings reach the mint pipeline — a
CLI arm that constructs an empty finding vector unconditionally is
a structural defect. The production `MintWalker` implementation
lives alongside the trait in `loom-workflow::mint::walk` and is
exercised by the CLI arm and by `loom loop`'s per-bead path.

**`loom gate verify --bead <id>`** runs all deterministic audits.
Marker parsing, `bd-closed` lookup, diff non-empty / empty, every
deterministic verifier (`[check]` / `[test]` / `[system]`) attached
to the bead's criteria (none short-circuits another), style linters
(`cargo clippy -- -D warnings`, `nix fmt --check`, `shellcheck`),
and any `[check]`-tier walks the consumer has registered for
cross-cutting structural audits.

Any `verify` failure routes into the existing hard-fail recovery
loop (`previous_failure` + `[loop] max_iterations`). Recovery doesn't
run `loom gate mint` for the same iteration — mechanical failure is
sufficient grounds.

**`loom gate mint --bead <id>`** walks the LLM rubric and mints
typed findings per the [*Findings and Minting*](#findings-and-minting)
contract. At per-diff scope the walk runs the LLM rubric only;
verify-side *failures* have already been handled as recovery context
by the loop above (they do not become Finding records at per-diff
scope — only at tree scope, where there's no recovery alternative).
Its inputs are:

- the diff
- the bead's intent (title, description, success criteria)
- the *full* touched spec sections (not only the bullets the diff lines map to)
- the diffs and criteria of sibling beads bonded to the same molecule
- deterministic-verifier sources (`[check]` walk implementations, `[test]` function bodies, `[system]` scripts)
- `[judge]` rubric texts
- `loom gate verify` results from the immediately-prior run

Rubric checks. The **Concern token** column lists the value the
reviewer emits as the `token` field in a `LOOM_FINDING: <json>`
line when the check fails (per [*Findings and
Minting*](#findings-and-minting)). The terminal `LOOM_CONCERN`
marker is emitted at end-of-walk if any findings were emitted (per
[harness.md § Verdict Gate](harness.md#verdict-gate)); it carries no
per-finding payload of its own. The `invariant-clash` token routes
to `loom:clarify` instead of recovery — see *Verdict* below. The
`scope-creep` / `scope-shortfall` tokens are per-bead only; the
tree-scope walk does not emit them.

| Check | Dimension | Lane | Concern token |
|---|---|---|---|
| **Conformance trace** — for every claim in touched spec sections, find a true code path (verifier-pass *or* LLM trace through current code). Scope includes the *full* touched spec sections — command-set tables, interface specs, decision tables, prose constraints — not only the bullets a diff line maps to. | Conformance | Hard fail | `spec-coherence-fail: <claim>` |
| **Contract closure** — for every multi-component contract the diff touches, verify completion in this diff or in bonded sibling diffs | Conformance | Hard fail | `orphan-integration: <contract>` |
| **Style-rule conformance** — diff complies with every rule in the consumer's pinned `{{ style_rules }}` document that linters cannot enforce mechanically. The judge discovers rule families from the document itself (no fixed prefix list — adapts to whatever convention the consuming project uses) and cites the rule id + file/line for each violation. | Style | Hard fail | `style-rule-violation: <rule-id>` |
| **Verifier honesty** — each deterministic verifier the diff adds or modifies (`[check]`, `[test]`, `[system]`) must support the claim it cites. Decomposed into four sub-checks; a verifier is honest iff it satisfies all four. (a) **verifier-bypass** — does the verifier actually exercise the live path? (b) **fabricated-result** — does the verifier's pass rely on a value the test itself synthesized? (c) **weak-assertion** — does the assertion meaningfully constrain the result, or does it tautologically pass? (d) **coincidental-pass** — does the verifier pass for the right reason, or because of an unrelated property of the system? The standing safety net re-checks existing verifiers against current spec/code to detect drift. **Pending-marked annotations (`[tier?](target)`) are exempt** — there is no verifier yet to be honest or dishonest about; honesty is re-evaluated the moment the implementing diff drops the `?`. | Test quality | Hard fail | `verifier-bypass: <verifier>` / `fabricated-result: <verifier>` / `weak-assertion: <verifier>` / `coincidental-pass: <verifier>` |
| **Mock discipline** — mocks have a discernible reason; mock isn't the thing under test | Test quality | Hard fail | `mock-discipline: <test>` |
| **Cross-component verifier sufficiency** — multi-component contracts need a verifier that exercises the seam, not one side | Test quality | Hard fail | `verifier-too-narrow: <criterion>` |
| **Concurrency coverage** — production code introducing or modifying `Mutex`/`RwLock`/`Arc<Mutex<T>>` etc. needs at least one concurrent-load test | Test quality | Hard fail | `concurrency-untested: <lock-site>` |
| **Scope appropriateness** — diff matches the bead's stated intent (title, description, success criteria) and the spec sections it claims to implement; touching files unrelated to that intent is creep, missing files necessary to satisfy the criteria is shortfall | Conformance | Hard fail | `scope-creep` / `scope-shortfall` |
| **`[judge]` rubrics** — work satisfies each LLM-judgement criterion on the bead | Conformance | Hard fail | `judge-flag: <criterion>` |
| **Invariant clash** — for each touched spec section (anchor + sibling), scan for load-bearing invariants the diff contradicts. The judge uses spec conventions as anchors (`## Out of Scope` and `## Non-Functional` sections, imperative-keyword sentences, architectural claims like *"X never calls Y"*, schema / data-structure declarations) and also catches prose-only invariants that lack a structural anchor. | (cross-cutting) | **Clarify** (three paths) | `invariant-clash: <invariant>` |

The style-rule-conformance check is the load-bearing defense for any
rule that cannot be expressed as a clippy / lint pattern. Most rules
in a typical `style-rules.md` are prose; the LLM judge is what
enforces them. The rubric requires walking the document concretely —
for every rule discovered in the pinned `{{ style_rules }}` document,
the judge either finds the supporting code or flags the violation
with the rule id. "Style looks fine" is not an acceptable answer; the
output must enumerate which rules were checked. The rule-family
prefixes vary per consuming project; the judge must adapt to whatever
the document uses rather than expecting a fixed set.

Verdict: any hard-fail finding → reviewer emits one `LOOM_FINDING:
<json>` line per finding + terminal `LOOM_CONCERN: {"summary":
"<one sentence>"}` → driver mints fix-up beads from the findings
(per [*Findings and Minting*](#findings-and-minting)) → verdict
gate routes to recovery loop with cause `review-concern`. The
terminal marker's payload is a JSON object with a single `summary`
field; routing is per-finding via the streamed `LOOM_FINDING:`
tokens, never via the terminal marker. The `invariant-clash` finding is
the exception: it produces a fix-up bead labelled `loom:clarify`
with a structured `## Options — …` block per the Options Format
Contract instead of a regular fix-up. The clarified bead is skipped
by `bd ready` on subsequent ticks; non-dependent beads in the
molecule continue running. Push is held until the clarify is
resolved via `loom msg` (see push-gate semantics in
[harness.md](harness.md#functional)).

### Standing-safety-net checks

`loom gate verify --tree`, `loom gate review --tree`, and `loom gate
mint --tree` form the standing-safety-net triad. The first two are
inspection-only and run independently (or compose via `loom gate
audit --tree`); the third is the act surface that produces fix-up
beads. Mechanical-only inspection is fast and frequent; the full
sweep + mint is rarer.

`loom gate verify --tree` exercises every audit at tree scope: every
`[check]` / `[test]` / `[system]` verifier, all linters, all
`[check]`-tier walks the consumer has registered, walking every spec
and every implementation file.

`loom gate review --tree` runs the LLM rubric against the whole spec
set × implementation. The checks from the per-diff rubric apply,
scoped to the tree rather than a diff. Additional safety-net-only
checks:

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
  `evidence` prose. Gated on `--tree` scope because per-diff scope
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
findings from both into fix-up beads. The walk semantics are
identical to `verify --tree` + `review --tree` running together; the
act is what `mint` adds. Standing-stage findings route per-spec via
the molecule lifecycle in
[harness.md](harness.md#molecule-lifecycle):

- **One open epic for the owning spec** — bond the fix-up bead to
  that epic. If the epic is part of an in-flight molecule, the
  fix-up extends it; iteration counters and base_commit cursors are
  unaffected.
- **No open epic** — mint molecule + epic (`bd create --type=epic
  --title="<X>" --labels="spec:<X>" --metadata
  "loom.base_commit=<HEAD>"`), then bond the fix-up. Each
  auto-create surfaces on stdout naming the spec and new epic ID.
  This is the safety property — findings about a spec with no
  active work get a fresh container, not silently dropped.
- **More than one open epic** — structural invariant violation (see
  [harness.md — Molecule lifecycle](harness.md#molecule-lifecycle)'s
  "at most one open epic per spec" rule). `mint` refuses to proceed
  and surfaces the conflicting epic IDs; the operator closes one
  before re-running.

See [*Findings and Minting*](#findings-and-minting) for the full
per-finding processing flow, dedup mechanism, and emit shape.

This behaviour is uniform whether `mint --tree` is invoked
workspace-wide, narrowed to one spec via `--spec <X>`, or
iterated per-spec by `loom loop --all-specs` — same resolution,
same outcome shape.

Invariant clashes surfaced at the standing safety net raise
`loom:clarify`.

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
one command that walks the rubric and produces fix-up beads. Every
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
Future protocols (agent stream-json, pi-mono RPC, run-phase exit
markers) may land as sibling modules (`loom-protocol::agent`,
`loom-protocol::run`, …) without re-litigating crate-extraction
overhead. Each protocol's wire-format major-bump SemVer is
governed at the module boundary by the same anti-drift pattern
`gate` uses (a single-source-of-truth partial paired with a
`[check]`-tier verifier that refuses restatement elsewhere; see
*Emit shape* below).

**Dependency direction.** Leaf crate. Depends on `serde` + `serde_json`
(JSON wire), `thiserror` / `displaydoc` (error types), and
`loom-events` for `SpecLabel`. No Askama, no bd client, no template
prose — those live one layer up. `loom-templates`, `loom-workflow`,
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
and `Finding::fingerprint`. The following stay `pub(crate)` so the
implementation can reshape without a major bump: per-layer validators
inside `Finding::validate`, per-variant `canonical_form` fingerprint
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

**Cross-repo consumers.** External consumers (e.g. wrapix) depend on
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
streamed `LOOM_FINDING:` line — the closed set of tokens
(`spec-coherence-fail`, `orphan-integration`, `verifier-bypass`,
…) the rubric emits and `loom gate mint` routes on. `ReviewConcern`
(in `loom-workflow::review::phase_verdict`) is a separate 12-variant
enum that previously named the terminal `LOOM_CONCERN` token; under
the retired terminal-token contract (per the per-bead rubric's
*Streaming + terminator pairing rule*), the terminal carries only
`{"summary": "..."}` and per-finding routing is decided on each
`LOOM_FINDING:` line's `ConcernToken`, not on the terminal.
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
per-bead `mint --bead` dispatch path.
[check](cargo run -p loom-walk -- audit_makes_no_bd_writes_outside_mint_module)

The driver's `loom loop` per-bead path is the **operator-level
composition** of `verify --bead` + `mint --bead` per the *Default
for bare invocation* table above; it is not a side-effect of any
inspection subcommand. The molecule-completion push gate runs
`audit` (verify + review) deliberately without minting fix-ups —
findings ride through the review-log file and through the typed
`PreviousFailure::ReviewConcern { summary, findings }` recovery
surface; an operator (or a subsequent `loom gate mint` invocation)
chooses when to consume them as bd state.

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
`LOOM_COMPLETE` / `LOOM_NOOP` / `LOOM_BLOCKED` / `LOOM_CLARIFY` are
bare because the parser reads context (reason / question) from the
prior non-empty line; LLM agents narrate the reason in prose and emit
the marker as a yes/no terminator without having to compose a JSON
object in the same turn. Mixing in either direction — JSON payload
for a bare marker, bare payload for a routed marker — is a
wire-format violation and is rejected by the typed parser
(`loom-workflow::todo::exit::parse_exit_signal` for terminals;
`loom-workflow::review::finding::parse_walk_output` for the streaming
finding lines).

### Scope-dependent walk

Mint's walk varies by scope flag:

| Scope | Walks | Why |
|---|---|---|
| `--bead <id>` | LLM rubric only | Deterministic failures on this bead's diff have already been handled by the preceding `verify --bead <id>` step in the loop (they route to recovery as `previous_failure` context, not Finding records). |
| `--diff <range>` | LLM rubric only | Symmetric with `--bead`; verify-side failures on a diff are the recovery loop's concern, not mint's. |
| `--files <paths>` | LLM rubric only | Same. |
| `--tree` | Deterministic verifiers + LLM rubric | No current bead to recover into; deterministic failures have no other home, so mint normalizes them into Finding records and mints fix-up beads from them. |

Bare `loom gate mint` (no scope flag) defaults to `--diff
<molecule.base_commit>..HEAD` if the active spec has an open epic;
else `--diff HEAD`. Same default as `audit` / `verify` / `review`
(`gate.md` *Scope flags* above).

### LOOM_INSIDE guard

`loom gate mint` refuses to run when `LOOM_INSIDE=1` (the same
guard `audit` respects). The bd writes from inside a
loom-managed container would mutate the driver's clone rather than
the local workdir — silently divergent state. The check is a
deterministic precondition; no walk runs and no exit code 2 path
fires.

### Concern tokens and target variants

Every finding carries a typed `target` whose variant is determined
by the `token`. The driver canonicalizes the variant when computing
the fingerprint (under *Fingerprint and dedup* below) so the same
finding hashes the same way across rubric runs.

| Token | Source | Target variant |
|---|---|---|
| `spec-coherence-fail` | Rubric (conformance trace) | `Criterion { spec, anchor }` |
| `orphan-integration` | Rubric (contract closure) | `Contract { id }` |
| `style-rule-violation` | Rubric (style-rule walk) | `StyleRule { rule_id }` |
| `verifier-bypass` / `weak-assertion` / `fabricated-result` / `coincidental-pass` | Rubric (verifier-honesty walk) | `Annotation { target_string }` |
| `mock-discipline` | Rubric | `TestPath { path }` |
| `verifier-too-narrow` | Rubric | `Criterion { spec, anchor }` |
| `concurrency-untested` | Rubric | `LockSite { file, line }` |
| `judge-flag` | Rubric (`[judge]` criterion) | `Criterion { spec, anchor }` |
| `invariant-clash` | Rubric (invariant-clash scan) | `Invariant { spec, section, tag }` |
| `template-spec-drift` | Rubric (tree-scope only) | `Template { path }` |
| `cross-spec-clash` | Rubric (tree-scope only) | `Criterion { spec, anchor }` |
| `spec-conventions-violation` | Rubric (tree-scope only) | `Criterion { spec, anchor }` |
| `verifier-failed` | Deterministic verifier exit ≠ 0 (tree-scope only) | `Annotation { target_string }` |
| `dispatch-error` | Verifier exit 2 — command not found / missing prereq (tree-scope only) | `Annotation { target_string }` |
| `unresolved-annotation` | Integrity gate forward-resolution (tree-scope only) | `Annotation { target_string }` |
| `stub-pointing` | Integrity gate stub-pointing (tree-scope only) | `Annotation { target_string }` |
| `multiple-annotations` | Integrity gate atomic-acceptance (tree-scope only) | `Criterion { spec, anchor }` |

`scope-creep` and `scope-shortfall` are per-bead tokens; the
tree-scope walk does not emit them, and mint never receives them
from a tree-scope source.

The target variant is architecture-bearing — its shape is what
makes "every finding carries a target appropriate to its token"
structurally unrepresentable as a mismatch. See [`spec-conventions.md`
*In scope #4*](../docs/spec-conventions.md).

### Emit shape

The LLM rubric walk emits findings as streaming lines on stdout
from the agent's subprocess. Each line is a `LOOM_FINDING:` prefix
followed by a JSON payload:

```
LOOM_FINDING: {"token":"<token>","bonds":["<spec>",...],"target":<target>,"evidence":"<evidence>"}
```

- **`token`** — concern identifier from the closed-set enum in
  *Concern tokens and target variants* above.
- **`bonds`** — array of spec labels the fix-up should bond to.
  Always present, always at least one element. The driver picks the
  bonding lead from this array via *Multi-spec findings* below.
- **`target`** — tagged JSON object whose `kind` discriminator
  selects the variant per the table above; carries
  identity-bearing fields specific to the variant.
- **`evidence`** — the rubric's reasoning, stored verbatim on the
  minted fix-up bead's description.

`bonds` is *bonding* metadata; `target` is *identity* metadata. The
two are kept separate so the driver can shift bonding (e.g., as
molecules open/close over time) without invalidating the
finding's fingerprint.

`<target>` shapes per variant:

```json
{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}
{"kind":"Contract","id":"molecule-lifecycle"}
{"kind":"StyleRule","rule_id":"RS-3"}
{"kind":"Annotation","target_string":"cargo test --lib parse_walks_all_md_files"}
{"kind":"TestPath","path":"crates/loom-gate/src/integrity.rs::test_x"}
{"kind":"LockSite","file":"crates/loom-workflow/src/run/runner.rs","line":210}
{"kind":"Invariant","spec":"harness","section":"Out of Scope","tag":"loom-runs-podman"}
{"kind":"Template","path":"crates/loom-templates/templates/review.md"}
```

One JSON object per line, emitted as the finding is identified
(not batched at end-of-walk). The driver parses each line
incrementally with a typed deserializer (`serde_json` into the
typed `Finding` struct), streaming rather than block-at-end. JSON
was chosen over pipe-delimited specifically because LLM emit is
more reliable on JSON than on bespoke formats — the target's
tagged-union shape encodes naturally, escaping is well-known, and
field-order independence avoids one class of malformed emit.

**Strict parse-time validation.** The `LOOM_FINDING:` prefix is
matched by substring search on each line of the agent's stdout, so
backtick-wrapped, markdown-fenced, or prose-prefixed lines are still
detected. The match is case-sensitive on the literal string
`LOOM_FINDING:` (with the trailing colon); bare-prose mentions
without the colon (e.g. *"the `LOOM_FINDING` marker"*) do not match
by design.

A line that matches the substring but fails the strict validation
that follows — malformed JSON (most common: trailing backticks from
markdown fencing), unknown `token`, any element of `bonds` that
doesn't resolve to a workspace spec label, `target` variant
mismatching `token`'s expected variant, or unresolved target content
(criterion anchor not in spec, file path absent on disk) — surfaces
as `BadWalk::MalformedFinding { errors, terminal }` per the
pairing-rule table below, with the well-formed terminal preserved
alongside the per-line errors. **No silent skip.** The substring-
then-strict-validate shape catches accidentally-fenced finding
emit while loudly typing the malformation, which is what makes the
wire format observable rather than fragile.

The walk is assumed to be retry-friendly: a re-run typically gets
the shape right; a persistently-malformed emit is signal that the
prompt or rubric needs adjusting.

Deterministic verifiers (at tree scope only) do **not** emit
`LOOM_FINDING:` lines — they continue to follow the existing
*Verifier-runner contract* (JSON verdict on stdout, exit codes).
The driver normalizes each failed verifier verdict into the same
typed Finding record the LLM rubric's lines parse into, then
applies the same mint flow uniformly. The mapping:

| Verifier outcome | `token` | `bonds` | `target` | `evidence` |
|---|---|---|---|---|
| `[check]` / `[test]` / `[system]` exit ≠ 0 (and ≠ 2, ≠ 77) | `verifier-failed` | `[<spec owning the annotation>]` | `Annotation { target_string }` | verifier's JSON `evidence` field, else stderr tail |
| Exit code 2 (dispatch error) | `dispatch-error` | same | `Annotation { target_string }` | command-not-found / missing-prereq message |
| Integrity gate: forward-resolution failure | `unresolved-annotation` | `[<spec owning the annotation>]` | `Annotation { target_string }` | "annotation does not resolve" with spec:line |
| Integrity gate: stub-pointing | `stub-pointing` | same | `Annotation { target_string }` | "annotation points at stub function" |
| Integrity gate: atomic-acceptance violation | `multiple-annotations` | same | `Criterion { spec, anchor }` | "criterion carries N annotations, expected 1" |
| Integrity gate: stale pending modifier | `unneeded-pending-marker` | same | `Annotation { target_string }` | "annotation is now resolved — drop the ? marker" with spec:line |

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
`LOOM_BLOCKED`, or `LOOM_CLARIFY`. `LOOM_BLOCKED` / `LOOM_CLARIFY`
mean the walk could not complete; see the marker definitions for
semantics. `LOOM_COMPLETE` and `LOOM_CONCERN` are the
verdict-carrying terminators and are governed by the pairing rule
below.

**`LOOM_CONCERN` payload — JSON shape and parse discipline.** The
payload is a JSON object with a single required field, `summary`,
whose value is a non-empty string:
`LOOM_CONCERN: {"summary": "<one-sentence summary>"}`. The driver
parses the payload with the same `serde_json` pipeline that
consumes `LOOM_FINDING:` lines. Parse failures — invalid JSON,
missing `summary`, empty `summary` — surface as the typed
`BadWalk::Concern { payload }` recovery cause (defined in
[harness.md](harness.md#verdict-gate)) so the recovery prompt can
carry the literal text that failed and the agent can fix the
shape on the next iteration. The summary is for the verdict log
only; the actionable detail lives in the streamed `LOOM_FINDING:`
lines, and per-finding routing is decided by `loom gate mint` on
each finding's token, not on the terminal marker. The terminal
token-and-reason form (`<token> -- <reason>`) is retired; the
terminal token only ever duplicated the strongest finding's token
at the cost of structural complexity.

**Streaming + terminator pairing rule.** The walk is a streaming
process: `LOOM_FINDING:` lines are emitted as concerns are
identified; the terminator is the final line. The driver
cross-checks the two — if the terminator and the stream disagree
on the walk's verdict, the run fails with a typed `BadWalk`
recovery cause:

| Finding stream | Terminator | Verdict |
|---|---|---|
| 0 | `LOOM_COMPLETE` | clean — phase done |
| ≥1 well-formed | `LOOM_CONCERN: {"summary":"..."}` | recovery — `RecoveryCause::ReviewConcern { summary, findings: Vec<Finding> }` threaded into `previous_failure` (mint consumes separately) |
| 0 | `LOOM_CONCERN: {"summary":"..."}` | `BadWalk::ConcernWithoutFindings { summary }` — concern claimed without enumeration |
| ≥1 well-formed | `LOOM_COMPLETE` | `BadWalk::FindingsWithoutConcern { finding_count, findings: Vec<Finding> }` — findings streamed but terminator claims clean; the parsed findings ride through so the next iteration's prompt can name them |
| ≥1 line failed parse | any | `BadWalk::MalformedFinding { errors: Vec<FindingParseError>, terminal: TerminalSurface }` — per-line errors are preserved alongside the typed terminal surface (well-formed terminal kept as-is; when the terminator also fails parse, the terminal is carried via `TerminalSurface::Malformed { payload }` so both failure pieces ride through the `MalformedFinding` variant) |
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
a concern, immediately emit a `LOOM_FINDING:` line with the
structured JSON detail and continue reviewing. When the walk is
complete, end your response with `LOOM_COMPLETE` if you found
nothing, or `LOOM_CONCERN: {"summary": "<one-sentence summary>"}` if you
emitted one or more `LOOM_FINDING:` lines. The terminator must
match the stream: `LOOM_COMPLETE` means zero findings,
`LOOM_CONCERN` means ≥1 finding.

**Single source of truth.** The wire-format definitions for both
`LOOM_FINDING:` and `LOOM_CONCERN:` live exactly once, in
`crates/loom-templates/templates/partial/findings_walk.md`. Other
templates that need to talk about these markers `{% include %}`
that partial; they never restate the format. The bare-marker
partials (`partial/progress_markers.md` for `LOOM_COMPLETE` /
`LOOM_NOOP`, `partial/self_report_markers.md` for `LOOM_BLOCKED` /
`LOOM_CLARIFY`) describe those markers' generic semantics; they
do not redefine the review-walk markers.

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

- **Stream-shape axis (4 cells):** zero `LOOM_FINDING:` lines; N
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
`LOOM_FINDING:` line → embed in a synthetic walk output with an
arbitrary well-formed terminator → `parse_walk_output` → assert
byte-equal to the input `Finding` and fingerprint identical.
Extends `loom-protocol::gate::tests::fingerprint_is_stable_across_runs_for_same_finding`
from "fingerprint stable" to "full struct round-trip."

### Fingerprint and dedup

Each finding is fingerprinted to prevent duplicate fix-ups across
mint invocations:

```
fingerprint = hash(token || canonical_form(target))[:12]
```

The fingerprint is **identity-only** — it depends on the token and
the target's canonical form. `bonds` is deliberately excluded
because bonding can shift across runs (a multi-spec finding's lead
shifts when its previously-lead spec's epic closes and lead-selection
falls to the next `bonds` element with an open epic; a single-spec
finding could pick a different `bonds[0]` on re-walk). Without this
exclusion, a stable finding would re-mint every time its bonding
context drifted.

`canonical_form` is variant-aware so `Criterion { spec: "gate",
anchor: "verifier-honesty" }` always serializes the same string
regardless of how the rubric phrased it. The choice of hash
algorithm, delimiter form, and exact 12-character length are
implementer's choice; what matters is that the result is stable
across rubric runs and fits in a bd label.

The fingerprint persists on the minted bead as a label:

```
loom:mint:<12-char-hash>
```

Before minting a finding, the driver queries bd:

```
bd query "label=loom:mint:<fingerprint> AND status=open"
```

- **Zero results** — proceed to mint.
- **One result** — skip (an open fix-up already exists for this
  finding); log in the run summary.
- **More than one** — structural violation (two beads share a
  fingerprint label); refuse and surface the conflicting IDs.

Closed-then-reopened semantics: the query is intentionally narrow
(`status=open`). A closed bead with the same fingerprint is *not*
re-minted on subsequent runs — operator silence after closure is
read as "decided not worth fixing." To force re-mint, the operator
removes the `loom:mint:<fp>` label (or deletes the bead). Reopening
alone does **not** force re-mint: the reopened bead still carries
the fingerprint, so the next dedup query matches an open bead and
skips. The narrow query is also what makes mint idempotent against
partial failure: a crash mid-run leaves the successfully-minted
beads with their labels; the next mint invocation's dedup query
skips them and retries only the ones that failed.

### Per-finding processing

For each `LOOM_FINDING:` line the driver receives:

1. **Parse** into typed fields: `{ token, bonds, target, evidence }`.
2. **Compute fingerprint** per the previous subsection
   (`hash(token || canonical_form(target))`; `bonds` is excluded).
3. **Dedup query** by `loom:mint:<fingerprint>` label. Zero results
   → continue; one open result → skip; more than one → refuse.
4. **Pick the bonding lead** from `bonds` per *Multi-spec findings*
   below — first element of `bonds` whose spec has an open epic; if
   none of the bonds have open epics, the lead is `bonds[0]` and
   step 5 mints a molecule + epic for it.
5. **Resolve the lead's molecule** via the single-tier query from
   [harness.md — Molecule lifecycle](harness.md#molecule-lifecycle):
   ```
   bd query "type=epic AND label=spec:<lead> AND status=open"
   ```
   - One result → use that epic.
   - Zero results → mint molecule + epic with `bd create
     --type=epic --title="<lead>" --labels="spec:<lead>"
     --metadata "loom.base_commit=<HEAD>"`.
   - More than one → structural violation, refuse.
6. **Mint the fix-up bead** with `bd create --type=task
   --parent=<epic-id> --labels="loom:mint:<fingerprint>,<spec-labels>"`,
   where `<spec-labels>` expands to one `spec:<X>` label per entry
   in `bonds` so cross-spec searches surface the bead from every
   named owner's perspective. Title and description shape is
   implementer's choice; the description must include the evidence
   string and the fingerprint, and the title must be stable across
   runs for the same finding (deterministic from token + target).
7. **`invariant-clash` carve-out** — instead of a regular fix-up
   bead, the minted bead also carries `loom:clarify` and its
   description embeds the canonical `## Options — …` block per the
   *Options Format Contract*. The rubric is responsible for emitting
   the options block as part of the finding's `evidence` payload.

End-of-run summary (printed to stdout, no bd writes): `minted M,
skipped K (dedup), refused R, errors E`, with per-finding fingerprint
and resulting bead id listed.

### Per-bead mint summary semantics

`loom gate mint --bead <id>` exits non-zero when `refused > 0` or
`errors > 0`; otherwise exit 0 even on `minted > 0` (creating fix-up
beads is a successful run). The driver — primarily `loom loop`'s
per-bead path — interprets the exit code and summary together:

- **Clean exit (refused == 0, errors == 0, any minted count).** The
  bead's work is done; whatever fix-ups were minted surface on the
  next outer-loop pass via `bd ready`. Per-bead path returns Done.
- **`refused > 0` — structural bd violation.** Refusal means either
  the dedup query (*Per-finding processing* step 3) returned >1 open
  bead sharing the fingerprint label, or the lead-resolution query
  (step 5) returned >1 open epic for the spec. Both are
  operator-resolvable bd states, not transient failures. The per-bead
  path routes the bead to `loom:blocked` with cause
  `mint-structural-violation` and the conflicting bead ids in the
  detail. Next `loom loop` pass after cleanup picks up where it left
  off via dedup re-resolve.
- **`errors > 0` — transient.** Unexpected failure during mint
  itself: bd CLI fault, subprocess timeout, or a `bd create` /
  `bd list` invocation that returned an unexpected shape. The per-
  bead path threads the summary's error detail into `PreviousFailure`
  and re-runs through the existing recovery loop bounded by
  `[loop] max_retries`. After the retry budget exhausts, the bead
  routes to `loom:clarify` with the accumulated error context — same
  recovery path the run-phase uses.

The bead's underlying work (the agent's run-phase) is unaffected by
this routing; mint runs *after* the agent signals Success per
*Per-diff stage checks* above. A `refused` mint outcome does not
unwind the agent's commit — it parks the bead so the operator can
resolve the bd-state issue without losing the work.

### Multi-spec findings

A finding can name more than one spec in `bonds` when the concern
spans seams (e.g., an `orphan-integration` contract spanning two
sibling specs). The `bonds` array is always present, always at
least one element; single-spec findings have a one-element array.

**Lead-spec selection rule.** The driver walks `bonds` in order
and picks the first element whose spec has an open epic. If none
of the bonds have an open epic, the lead is `bonds[0]` and mint
creates a molecule + epic for it. This treats the rubric's
ordering as authoritative for primacy while preferring existing
molecules over creating new ones.

The fix-up bead is `--parent`-ed to the lead spec's epic and
carries one `spec:<X>` label per `bonds[i]` so cross-spec searches
surface it from every named owner's perspective.

**Bonding shifts are not identity shifts.** Because the
fingerprint depends only on `(token, canonical_form(target))` and
*not* on `bonds`, a finding's identity is stable even when the
bonding context changes between runs (e.g., a new spec joins
`bonds`, or an open epic closes). The existing fix-up bead remains
in its original molecule; subsequent walks see the dedup label and
skip re-minting. Lead-selection is only consulted at first-mint
time.

**Validation rule.** For target variants that carry a `spec`
field (`Criterion` and `Invariant`), `target.spec` MUST appear in
`bonds` — the rubric cannot cite a criterion or invariant in spec
X while bonding only to spec Y. Validation failure rejects the
finding with a typed parse error.

## Marker

`MarkerProof` is the content-addressed trust-bearing artifact the
verdict gate mints on audit-pass and prek's pre-push hook consumes
to short-circuit redundant work on driver-loop integration pushes.
Its purpose is to make "the gate ran cleanly at this exact
workspace state" a typed, forgery-resistant Rust value rather than
an ad-hoc filesystem stamp.

### Type-safe mint

The mint authority lives in `loom-gate::marker`. The constructor
`MarkerProof::from_gate_success` is `pub(crate)`, accepts a sealed
`GateSuccess` (defined in [harness.md § Loop Outcome
Types](harness.md#loop-outcome-types) and itself `pub(crate)`),
computes the current workspace fingerprint, and returns a
`MarkerProof` value. No code path outside `loom-gate::marker` can
mint a marker; no code path outside the gate-invocation module can
construct the `GateSuccess` that mint requires. The agent in the
bead container cannot mint, regardless of what it writes to disk
or emits on stdout.

```rust
pub struct MarkerProof {
    version: u32,                    // schema version (currently 1)
    commit_sha: GitOid,              // HEAD SHA — informational
    tree_oid: GitOid,                // HEAD's tree OID — the fingerprint
    minted_at: SystemTime,
}

impl MarkerProof {
    /// Mint authority — `pub(crate)`. Takes a sealed `GateSuccess`,
    /// computes the workspace fingerprint at mint time.
    pub(crate) fn from_gate_success(
        s: GateSuccess,
        workspace: &Path,
    ) -> Result<Self, MintError>;

    /// Atomic write to disk: `<path>.tmp` + rename.
    pub(crate) fn write_to(&self, path: &Path) -> Result<(), io::Error>;

    /// The only validated read constructor. Deserializes, computes
    /// the current workspace fingerprint, returns `Ok` iff the
    /// marker's `tree_oid` matches the current tree OID, porcelain
    /// is clean, and the schema version is supported.
    pub fn read_and_validate(
        path: &Path,
        workspace: &Path,
    ) -> Result<Self, MarkerError>;
}
```

A value of type `MarkerProof` anywhere in the code corresponds to
"the gate ran AND the workspace still matches at the moment this
value was constructed" — by construction. The deserializer cannot
yield a `MarkerProof` for a stale or mismatched state; it returns
`Err`.

### Fingerprint contents

`WorkspaceFingerprint` is the git tree OID at HEAD, with a
porcelain-clean precondition. Validation is:

```rust
fn validate(workspace: &Path, marker: &MarkerProof) -> Result<(), MarkerError> {
    assert_porcelain_clean(workspace)?;          // working tree == HEAD's tree
    let current_tree = git_tree_oid_of_head(workspace)?;
    if current_tree != marker.tree_oid { return Err(FingerprintMismatch); }
    if marker.version > CURRENT_VERSION { return Err(UnsupportedSchema); }
    Ok(())
}
```

Three checks: porcelain clean (no uncommitted edits), tree OID
match, schema version supported. Toolchain files
(`rust-toolchain.toml`, `flake.lock`, `Cargo.lock`) are tracked,
so their content folds into the tree OID; toolchain bumps
invalidate the fingerprint naturally.

The tree OID is git's canonical hash of HEAD's tree state, so we
don't walk `git ls-files` or compute a custom hash. The
fingerprint is structurally derived from git's own object store.

### File location and lifecycle

Marker lives at `.loom/marker.json` in the loom workspace —
a single file, overwritten on each mint. Atomic write via
`<path>.tmp` + rename. No per-commit history (debuggable via
`loom logs` and bd notes), no sweeping needed. The file lives in
the loom workspace only; operator and bead workspaces do not
contain a `marker.json`.

```json
{
  "version": 1,
  "commit_sha": "<HEAD SHA — informational>",
  "tree_oid": "<tree OID — the fingerprint>",
  "minted_at_ms": 1234567890123
}
```

### Mint trigger

The driver-side molecule-completion push gate at the loom workspace
(per [harness.md § Verdict Gate](harness.md#verdict-gate)) is the
sole mint trigger. By the time the push gate runs, every bead in
the molecule has already integrated via its own per-bead
verdict-gate pass (each of which rebased + ff'd onto the
integration branch, ran verify against the integrated tree, and
released the lock). The push gate sees the cumulative integrated
state at HEAD; it does not rebase. The mint sequence is:

1. Push gate acquires `index.lock`.
2. Runs full audit against the integrated tree:
   `prek run --hook-stage pre-push --all-files` +
   `loom gate review --diff <molecule.base_commit>..HEAD`.
3. On audit-pass, constructs `GateSuccess`; calls
   `MarkerProof::from_gate_success(success, loom_workspace)`;
   `write_to(".loom/marker.json")`.
4. Runs `git push origin <integration-branch>` — still inside the
   critical section.
5. Releases `index.lock`.

The critical section spans audit + mint + push so that no
in-flight per-bead integration can mutate HEAD between mint and
push. Per-bead integration steps acquire their own (separate) hold
of `index.lock` for the rebase + ff + verify pass; they release
without minting. The two stages contend for the same lock; the
push gate waits for any in-flight per-bead integration to release
before starting its own critical section.

### Consumer contract

`loom gate verify-marker` is the prek-side consumer. It calls
`MarkerProof::read_and_validate(".loom/marker.json", ".")`
and exits 0 on `Ok`, non-zero on `Err`. The exit code is the
contract; the diagnostic on stderr names the specific
`MarkerError` variant for human debugging but is not part of the
machine-readable contract.

prek's pre-push chain positions `loom gate verify-marker` as the
first hook. Each subsequent slow-tier hook's `entry` routes
through a wrapper (`pre-push-checks`, owned upstream by
`wrapix.prekHooks` — see [pre-commit.md § Marker
integration](pre-commit.md)) that re-runs the marker check and
short-circuits the underlying command on valid marker. On invalid
or missing marker, the wrapper execs the underlying command and
the slow tier runs normally.

### Forgery resistance

The marker is forgery-resistant against three threat shapes:

- **Tree-state forgery.** Cannot claim to have verified state X and
  ship state Y: the tree OID fingerprint binds the marker to a
  specific tree. Mismatch → validation fails.
- **Stale marker after edit.** Uncommitted edits invalidate via the
  porcelain-clean precondition; new commits invalidate via the tree
  OID change. The marker is single-bit and binds to the exact
  HEAD's tree.
- **Agent verifier-execution forgery.** The bead-container agent
  cannot mint a `MarkerProof` because the constructor requires a
  sealed `GateSuccess`, and `GateSuccess` is `pub(crate)` to the
  driver's gate-invocation module. An agent that writes a
  hand-crafted JSON file to `.loom/marker.json` produces a
  file that deserializes to `MarkerProof` only insofar as the JSON
  shape matches; the validation step still recomputes the
  workspace fingerprint and matches against the file's claimed
  fingerprint. The agent can't forge the JSON's `tree_oid` to a
  value not equal to the current HEAD's tree OID (validation would
  catch the mismatch unless the agent first mutates HEAD's tree to
  match — which would itself be the agent making the changes it
  claimed to have verified, and prek's downstream hooks would
  catch the unverified state). The chain holds.

### Cross-workspace boundary

The marker is workspace-local — never trusted across machines or
across clones of the same repo. The driver's loom workspace, the
operator's `/workspace`, and the bead workspaces are separate
clones; each has its own `.loom/marker.json` (or doesn't).
The only writer is the driver-side verdict gate in the loom
workspace; operator and bead workspaces never have a marker, so
their prek pre-push falls through to the full slow tier.

CI never reads the marker. CI's nix-pure sandbox re-derives every
check; the marker is a within-workspace driver-loop optimization,
not a cross-machine trust artifact.

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
| `[check]` | `[check](command)` — shell command | Each annotation invokes its own process (often a walk binary the consumer ships). |
| `[test]` | `[test](path)` — language-native test path (e.g. `crate::module::test_name`, `tests/test_foo.py::test_bar`) | The gate collects all `[test]` targets in a single `loom gate test` invocation and issues **one** runner subprocess (e.g. `cargo nextest run -E 'test(p1) \| test(p2) \| ...'`). One process per invocation, full internal parallelism. |
| `[system]` | `[system](command)` — shell command | Each annotation invokes its own process. System verifiers are inherently slow and self-contained; batching doesn't help. |
| `[judge]` | `[judge](path)` — file path or criterion id whose content is the LLM rubric | The gate collects all `[judge]` targets and issues concurrent LLM calls (API-level parallelism). |

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
diff or the push gate refuses on `UnneededPendingMarker`. This
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

### Runners — per-language batched dispatch

**Runners, not verifiers, are the dispatch unit.** A runner executes
one batch of annotations in a single subprocess. Per-language
batching avoids the "process per test" cost that dominates wall-clock
on non-trivial specs.

The dispatcher's job:

1. Collect all in-scope annotations (per *Verifier inputs* + the
   scope flag's input set, intersected).
2. Group by which runner matches them.
3. For each runner with a batch template, build one command, spawn
   once, parse per-target verdicts from the output.
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

#### Verifier inputs

Every verifier declares the **files it examines** — the gate uses
these declarations to decide whether to run the verifier given a
scope's input set. The intersection rule is: verifier runs iff
`declared inputs ∩ scope input set ≠ ∅`.

The wire format is a list of **gitignore-style glob patterns
relative to repo root**. Where the declarations come from depends
on verifier kind:

| Verifier kind | Source of inputs |
|---|---|
| `[test](name)` | Derived from test framework metadata. For Rust: walk `cargo metadata`, resolve the test's owning crate, declare the crate's source dirs. For pytest: pytest's collection output. For other frameworks: `<workspace>/loom.toml` `[runner.<tier>] inputs_for_test = "<command>"`. |
| `[check]` / `[system]` referencing a **script** | A `# loom-inputs: <comma-separated globs>` header line in the script. Format is uniform across script languages — the line is found by literal-string search, not by interpreting shebangs. |
| `[check]` / `[system]` referencing a **binary** that supports the input-query protocol | The binary returns inputs via `<binary> --print-inputs <remaining-argv>` printing JSON `{"inputs": ["glob1", "glob2"]}` to stdout. |
| `[check]` / `[system]` — fallback | Heuristic path extraction from the command string. `grep -q 'X' path/to/file` → `path/to/file`. `cargo test -p mycrate --lib testname` → `mycrate`'s sources via cargo metadata. Conservative; misses are caught by the standing-stage safety-net sweep. |
| `[judge](script#fn)` | A `# loom-inputs:` header line in the judge script (same convention as `[check]`/`[system]` scripts). |

**Spec-section auto-include.** The spec section the annotation lives
in is *always* part of the verifier's inputs. The gate adds it
automatically; spec authors don't declare it. Editing the spec
section re-runs the verifier without anyone writing a rule.

**Empty inputs are a smell.** A verifier that examines nothing under
the repo is either a misdeclaration or a no-op. Genuinely
cross-cutting verifiers declare **broad** inputs (e.g. integrity
gate declares "every spec file in the input set"; workspace lints
declare every workspace `Cargo.toml`), not empty. The standing-stage
safety net surfaces unintentional empties.

**Repo-agnostic.** The `# loom-inputs:` header works in any script
language. The `--print-inputs` convention works for any binary. The
`[runner.<tier>] inputs_for_test` config knob handles non-default
test frameworks. Loom-the-library has no privileged knowledge of
any consumer's layout.

Spec annotations stay **clean** — `[tier](target)` and nothing else.
No inline metadata, no HTML-comment companions, no syntax
extensions. Override mechanisms live next to the verifier (script
header, binary protocol, runner config), not next to the
annotation.

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

For batched tiers, the gate filters annotations to those whose
scope intersects `--files` before issuing the batched invocation:

- `[test]`-tier scope = files in `crate(test)` ∪ files in
  `crate(test)`'s transitive dependencies (Rust; computed via
  `cargo metadata`). Other toolchains supply analogous mappings.
- For non-batched tiers (`[check]`, `[system]`), the gate passes
  `LOOM_FILES` as env and the verifier decides whether to filter.
  Most verifiers can be dumb (run the same way regardless); walks
  that benefit from scope filtering read the env var.

### Test-tier silent-zero-match

`cargo test -- some_name` and equivalents in other runners exit 0
silently when no test matches the filter. The gate sniffs known
runners (`cargo test`, `cargo nextest`, `pytest`) and post-processes
output to detect zero-match cases, failing the run with a clear
error. Consumers using unrecognised runners must ensure their
runner fails on zero-match.

## Integrity gate

The deterministic gate that verifies the annotations themselves
resolve. Runs as part of `loom gate check`. Three directions:

1. **Forward — every annotation's target is valid for its tier.**
   - `[check](cmd)` and `[system](cmd)`: the command's first token
     resolves on PATH or as a file in the repo (best-effort —
     dynamic commands may resolve only at runtime).
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

Failure output (one per finding):

- `<spec>:<line>: annotation [tier](<target>) — does not resolve`
- `<spec>:<line>: criterion carries N annotations, expected 1`
- `<spec>:<line>: annotation [tier](<target>) points at stub function`
- `<spec>:<line>: annotation [tier?](<target>) is now resolved — drop the ? marker`

**Integrity findings are terminal at the push gate** (harness.md
FR9). `UnresolvedAnnotation`, `StubTestFunction`, and
`UnneededPendingMarker` findings within the molecule's diff scope
refuse the push and apply `loom:clarify` to the molecule's epic
with an auto-generated `## Options — …` block per the *Options
Format Contract* above.

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

`loom gate status` reads from a sqlite-backed status cache and prints
a fast report. (Bare `loom gate` shows the subcommand help — see
*Commands* above.) Every subcommand that runs verifiers or the LLM
rubric writes to the cache as it runs — `loom gate verify`, `loom
gate review`, `loom gate audit`, the tier subcommands (`check` /
`test` / `system` / `judge` / `rubric`), and `loom gate mint` (via
its embedded verify and rubric walks).

**Cache contents per criterion:**
- annotation target
- last-run timestamp and commit hash
- pass / fail / skipped verdict (`skipped` covers scope-filter
  exclusion and verifier-reported prerequisite gaps via exit 77)
- evidence string from the verifier's JSON output

**Cache schema** extends the existing state-db schema in
[harness.md](harness.md). One row per criterion, indexed
by `(spec_label, criterion_anchor)`.

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
`loom msg` can consume mechanically:

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

`loom msg` consumes this format three ways:

- **List mode** (`loom msg`): the `## Options — <summary>` line is
  rendered as the bead's SUMMARY column.
- **View mode** (`loom msg -n <N>` / `loom msg -b <id>`): the full
  block is rendered to the user with each `### Option N` heading.
- **Fast-reply** (`loom msg -n <N> -o <K>`): the body of `### Option
  K` is recorded as the resolution note, the bead is closed, and
  `loom:clarify` is removed.

A clarify bead can present fewer or differently-framed options when
the decision warrants — the format is `### Option <integer> —
<title>` for any integer ≥ 1. The summary line is always required.

**Persistence boundary: agent narrates, agent persists.** The gate
does not parse the reviewer's stdout for `## Options` / `### Option
N` blocks — neither `loom gate verify`/`review` nor the verdict-gate
phase classifier (`phase_verdict::decide`) scrapes prose for
options. The reviewing agent is the only mechanism that puts the
canonical block into bead state, via one of:

- `bd create … --description "<options block>"` when the clarify is
  a new bead, OR
- `bd update <id> --notes "<options block>" && bd update <id>
  --add-label=loom:clarify` when the options apply to an already-
  existing bead (e.g. promoting a previously `loom:blocked` bead to
  `loom:clarify` once the reviewer enumerates unblock paths).

The agent must complete the `bd` write **before** emitting
`LOOM_COMPLETE` / `LOOM_CONCERN`. Reviewer prose that names
options without a corresponding `bd` write leaves the canonical
block in the review log file only — `loom msg`'s queue stays empty
and the downstream user cannot fast-reply. The reviewer template
in `loom-templates/templates/review.md` documents the required
`bd` invocations.

### Resolution lifecycle

The `## Options — <summary>` block lives in the target bead's
notes only from emit to resolution. When `loom:clarify` is
cleared — via `loom msg -o`, `-r`, `-d`, or the chat session's
`bd update --remove-label=loom:clarify` — the originating
options block is removed from notes in the same transaction that
records the resolution note (chosen option body, verbatim reply,
or dismissal note). The resolution replaces the question on the
bead's notes record.

A single bead can receive multiple clarifications across its
lifetime — notably the molecule epic, which hosts
decomposition-phase clarifies emitted by successive `loom todo`
invocations as well as push-gate clarifies. Without removal,
options blocks accumulate and `loom msg` lists become ambiguous
about which block belongs to the currently active label.

For clarifies hosted on a **dedicated clarify bead** (created
via `bd create` per the Persistence boundary above, closed on
fast-reply per `loom msg`'s consumption shape above), the
removal is moot — the whole bead is closed and the notes pass
out of scope with it. The lifecycle contract is load-bearing
for the **existing-bead** path (`bd update --notes` +
`--add-label=loom:clarify`) where the bead survives the
resolution.

## Output

The gate's output is a verdict (pass / hard-fail / clarify) plus any
flagged actions. There is no separate persistence layer — `bd` issues
and git commits already provide the durable record:

- **Per-diff verify failures** drive the existing recovery loop
  with `previous_failure` context. They do not produce Finding
  records or fix-up beads.
- **Rubric findings** (per-diff via `mint --bead`) and
  **deterministic + rubric findings** (tree scope via `mint --tree`)
  are minted into fix-up beads, bonded per-spec via the molecule
  lifecycle (see [*Findings and Minting*](#findings-and-minting)).
- **`invariant-clash` findings** additionally carry `loom:clarify`
  on the minted bead, with a canonical `## Options — …` block per
  the Options Format Contract.

Past gate runs are not persisted; *past passes don't grant immunity
from re-evaluation*. Conformance is a property of the current
code-spec pair, not a historical fact. Observability of gate
behaviour over time, if needed, is added separately and is not part
of this spec.

## Recovery

Per-stage flag handling:

- **Plan** — interview held until the spec is amended (claim
  surfaced, clash resolved, or explicitly out-of-scope'd). User
  authorisation required to ship a spec with unresolved gaps.
- **Per-diff** — hard-fail flags enter the existing recovery loop
  bounded by `[loop] max_iterations` with `previous_failure`
  rendered into the next prompt. All iterations except the last
  are same-agent retries with cumulative `previous_failure`; the
  final iteration uses a **fresh agent**: new container, new agent
  process, blank scratchpad; receives the spec, the bead's
  criteria, the cumulative `previous_failure`, and the current
  state of the worktree — but *not* the prior session's transcript
  or in-memory context. Rationale: same-agent retry has a low
  recovery rate and a high re-fail rate; the final attempt gets
  failure evidence without the failed approach. Invariant clashes
  follow a different path: mint creates a new fix-up bead carrying
  `loom:clarify` (per *Findings and Minting*), and `bd ready`
  skips that fix-up bead on subsequent ticks while non-dependent
  beads in the molecule continue running. `loom msg` resolves the
  clarify on the fix-up bead. Clashes never trigger fresh-agent
  retry of the bead that surfaced them.
- **Standing** — `loom gate mint --tree` walks the deterministic
  verifiers and the LLM rubric, mints typed findings as fix-up
  beads bonded to each owning spec's open epic (resolved via single
  bd query per *Standing-safety-net checks* above); a fresh
  molecule + epic is minted when no open epic exists for the spec.
  Invariant clashes surface via `loom:clarify` on the minted
  fix-up bead; resolved in the next `loom msg` walk. See
  [*Findings and Minting*](#findings-and-minting) for the
  per-finding processing flow.

### Post-hoc recovery — when the push gate was skipped

**Use case.** A molecule's beads closed without `GateSuccess` being
constructed — e.g., a legacy run from before the type-shape
enforcement landed, or a manual `bd close` outside the gate. The
work shipped but was never audited; the codebase has unverified
divergence from the spec. The push-stage scope (`<molecule.base_commit>..HEAD`)
no longer applies because HEAD has moved on to subsequent
(also-unaudited) work, and the molecule's `loom.base_commit` would
include unrelated downstream commits.

**Canonical recovery path:** `loom gate mint --tree`, single-spec
or multi-spec. The standing-safety-net scope is exactly what's
needed — walk the full spec(s) against the full implementation, no
diff math, no dependence on a still-valid `loom.base_commit`, with
fix-up beads minted per-spec as findings emerge.

```bash
# Single spec
loom gate mint --tree --spec <label>   # standing walk; mints fix-ups to the
                                       #   spec's open epic, or mints molecule+
                                       #   epic if absent
loom loop                              # process the resulting fix-up beads; gate
                                       #   fires structurally on completion

# Across every spec in the workspace
loom gate mint --tree                  # walks every spec; mints molecule+epic
                                       #   where no open epic exists
loom loop --all-specs                  # iterates each spec with an open epic
```

For inspection without minting, `loom gate audit --tree` runs the
same walk and prints findings to stdout without bd writes.

No explicit seeding step is required — mint resolves the target
epic via single bd query (per [harness.md — Molecule
lifecycle](harness.md#molecule-lifecycle)) and mints a fresh
molecule when the lookup returns nothing. This is the same
single-tier resolution the standing safety net uses; recovery is
just the safety net exercised explicitly.

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
integrity gate's self-referential tests (under *Integrity gate — three
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

### Integrity gate — three directions

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

- `loom gate mint --tree --spec <X>` resolves the bonding target
  via single bd query (per [harness.md — Molecule
  lifecycle](harness.md#molecule-lifecycle)); zero results → mints
  molecule + epic and bonds fix-ups to the new molecule; one result
  → bonds fix-ups to its molecule; more than one → structural
  invariant violation, refuses to proceed and surfaces the
  conflicting epic IDs
  [test](mint_tree_scope_resolves_lead_spec_via_single_tier_query)
- `loom gate mint --tree` (all-specs sweep) applies the same
  single-tier resolution per spec. Each spec independently mints
  its own molecule + epic when the bd query returns zero, or bonds
  to the existing molecule when it returns one. Each auto-create
  surfaces on stdout naming the spec and new epic ID. No pointer
  table is written
  [test](mint_tree_scope_per_spec_resolution_does_not_clobber_existing_epics)
- `loom gate audit --tree` is inspection-only: it walks the same
  rubric `mint --tree` walks and prints findings to stdout, but
  produces zero bd writes
  [test](audit_tree_scope_makes_no_bd_writes)

### Findings and Minting

- `loom gate mint` refuses to run when `LOOM_INSIDE=1`, exiting
  non-zero with a deterministic error message and producing no bd
  writes
  [test](mint_refuses_when_loom_inside_env_is_set)
- The walk emits `LOOM_FINDING: <json>` lines on stdout, one
  JSON object per finding, streamed as findings are identified
  (not batched at end-of-walk). The JSON shape is `{"token": ...,
  "bonds": [...], "target": {"kind": ..., ...}, "evidence": ...}`
  [test](mint_walk_emits_loom_finding_json_lines_streamed_per_finding)
- The walk terminates with exactly one of `LOOM_COMPLETE`,
  `LOOM_CONCERN: {"summary": "..."}`, `LOOM_BLOCKED`, or `LOOM_CLARIFY`;
  a walk that emits `LOOM_FINDING:` lines without a terminal marker
  fails the mint invocation with non-zero exit
  [test](mint_walk_without_terminal_marker_fails_run)
- `LOOM_CONCERN:` payload parses as JSON `{"summary": "<non-empty
  string>"}` via the same `serde_json` pipeline that consumes
  `LOOM_FINDING:` lines; the parsed summary becomes the verdict-log
  entry for the walk
  [test](concern_payload_parses_as_json_with_summary_field)
- Parse failures on the `LOOM_CONCERN:` payload — invalid JSON, missing
  `summary` field, empty `summary` string — surface as
  `RecoveryCause::BadWalk(BadWalk::Concern { payload })` carrying the
  literal post-marker text so the recovery prompt can quote it back
  to the agent
  [test](concern_malformed_payload_routes_to_bad_walk_concern_with_literal_payload)
- A walk that emits `LOOM_CONCERN:` with zero preceding `LOOM_FINDING:`
  lines surfaces as `RecoveryCause::BadWalk(BadWalk::ConcernWithoutFindings
  { summary })` — concern claimed without enumeration
  [test](concern_without_streamed_findings_routes_to_badwalk_concern_without_findings)
- A walk that streams one or more `LOOM_FINDING:` lines and terminates
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
- A malformed `LOOM_FINDING:` line — invalid JSON, unknown token,
  unknown spec, target variant mismatching token, or unresolved
  target content — fails the mint invocation with a typed parse
  error naming the offending line; no silent skip
  [test](mint_malformed_loom_finding_fails_run_with_typed_error)
- The dedup query (`bd query "label=loom:mint:<fp> AND status=open"`)
  returning one open result causes the finding to be skipped
  [test](mint_dedup_query_one_open_result_skips_finding)
- The dedup query returning zero results proceeds to mint
  [test](mint_dedup_query_zero_results_proceeds_to_mint)
- The dedup query returning more than one open result is refused
  as a structural violation
  [test](mint_dedup_query_multiple_open_results_refuses_as_structural_violation)
- A closed bead carrying the same fingerprint label is not
  re-minted on subsequent runs; only removing the `loom:mint:<fp>`
  label or deleting the bead forces re-mint
  [test](mint_dedup_does_not_re_mint_closed_bead_with_same_fingerprint)
- Reopening a closed bead does not force re-mint — the reopened
  bead still carries the fingerprint and dedups against itself
  [test](mint_dedup_skips_reopened_bead_still_carrying_fingerprint_label)
- The fingerprint hash is stable across rubric runs for the same
  finding (same `token`, same canonicalized `target` → same
  12-character hash, regardless of how `bonds` is ordered or which
  spec ultimately wins lead-selection)
  [test?](mint_fingerprint_is_stable_across_rubric_runs_for_same_finding)
- The minted fix-up bead is `--parent`-ed to the lead spec's open
  epic and carries the `loom:mint:<fingerprint>` label
  [test](mint_creates_fixup_with_parent_epic_and_fingerprint_label)
- The bonding lead is the first element of the finding's `bonds`
  array whose spec has an open epic; if none of the bonds have an
  open epic, the lead is `bonds[0]` and mint creates a molecule +
  epic for it. The minted fix-up carries one `spec:<X>` label per
  element of `bonds` so cross-spec searches surface it from every
  owner's perspective
  [test](mint_bonding_lead_is_first_bonds_element_with_open_epic)
- A finding's fingerprint depends on `token` and
  `canonical_form(target)` only — never on `bonds`. The same
  finding emitted on a re-run with a different bonds-array
  ordering or a different lead-spec resolves to the same
  fingerprint and dedups against the existing fix-up bead
  [test?](mint_fingerprint_excludes_bonds_so_bonding_shifts_do_not_remint)
- For target variants that carry a spec field (currently only
  `Criterion`), `target.spec` MUST appear in `bonds`; a finding
  that violates this is rejected with a typed parse error
  [test](mint_rejects_criterion_target_whose_spec_is_not_in_bonds)
- `invariant-clash` findings mint a fix-up bead carrying both
  `loom:mint:<fp>` and `loom:clarify` labels, with the description
  embedding a canonical `## Options — …` block per the *Options
  Format Contract*
  [test](mint_invariant_clash_finding_creates_fixup_with_clarify_label_and_options_block)
- `mint --bead <id>` walks the LLM rubric only, not the deterministic
  verifiers; verify-side findings have already been handled by the
  preceding `verify --bead <id>` step in the loop
  [test](mint_bead_scope_walks_llm_rubric_only_not_verifiers)
- `mint --tree` walks both the deterministic verifiers and the LLM
  rubric, emitting `LOOM_FINDING:` lines for findings from either
  source
  [test](mint_tree_scope_walks_verifiers_and_rubric_emitting_findings_from_both)
- Mint is idempotent against partial failure: a crash mid-run leaves
  the successfully-minted beads with their fingerprint labels; a
  re-run's dedup query skips them and retries only the unfinished
  findings
  [test](mint_idempotent_after_partial_failure_retries_only_unfinished_findings)
- `mint --dry-run` walks the rubric, prints proposed bd writes to
  stdout, and makes zero bd writes
  [test](mint_dry_run_makes_no_bd_writes)
- `mint --spec <X>` filters findings to those whose lead-spec
  resolves to `<X>` after multi-spec lead selection; findings
  routing to other specs are reported as skipped
  [test](mint_spec_filter_drops_findings_routing_to_other_specs)
- Bare `loom gate mint` (no scope flag) defaults to `--diff
  <molecule.base_commit>..HEAD` when the active spec has an open
  epic, else `--diff HEAD` — same default policy as `audit` /
  `verify` / `review`
  [test](mint_bare_invocation_defaults_to_active_molecule_diff)
- The end-of-run summary lists minted, skipped-dedup, refused, and
  errored counts, with per-finding fingerprint and resulting bead
  id
  [test](mint_end_of_run_summary_reports_per_finding_outcomes)

### Wire-format wiring and dead-code excision

The production wiring obligation — every caller that constructs
`GateInputs` for the review-phase verdict gate must populate
`streamed_findings_count` from a real `parse_walk_output` invocation
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

- A `LOOM_FINDING:` line whose JSON payload fails parse — invalid
  JSON (most common: trailing backticks from markdown fencing),
  unknown `token`, target/token variant mismatch, unresolved spec
  label or anchor — surfaces as
  `RecoveryCause::BadWalk(BadWalk::MalformedFinding { errors, terminal })`
  with the well-formed terminal preserved alongside the per-line
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
  carries every per-line parse error AND the well-formed terminal
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
  `serde_json::to_string` → embed in a `LOOM_FINDING:` line →
  embed in a synthetic walk output → `parse_walk_output`, with
  stable fingerprint
  [test](every_finding_round_trips_through_wire_format_with_stable_fingerprint)
- `ConcernToken::CrossSpecClash` round-trips through the wire format
  with canonical target `Criterion { spec, anchor }` and is exercised
  by the round-trip property test cell set
  [test?](concern_token_cross_spec_clash_round_trips_with_criterion_target)
- `ConcernToken::SpecConventionsViolation` round-trips through the
  wire format with canonical target `Criterion { spec, anchor }` and
  is exercised by the round-trip property test cell set
  [test?](concern_token_spec_conventions_violation_round_trips_with_criterion_target)
- `cross-spec-clash` and `spec-conventions-violation` are
  tree-scope-only tokens: the rubric emits them at `--tree` scope;
  `--bead` / `--diff` / `--files` scope rejects them. A finding
  carrying either token parsed at non-tree scope surfaces a typed
  `FindingParseError` variant naming the scope mismatch, alongside
  the existing per-bead-only restriction on `scope-creep` /
  `scope-shortfall`
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
  `displaydoc`, and `loom-events` (for `SpecLabel`); no transitive
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

The seam between `loom gate mint`'s CLI arm and the underlying walker
is the `MintWalker` trait. The CLI arm dispatches through the trait
so findings reach the mint pipeline from a real walk. A CLI arm
that constructs an empty finding vector unconditionally — bypassing
the walker — is a structural defect; the walker is the only path
findings reach mint.

- A production `MintWalker` implementation exists in
  `loom-workflow::mint::walk` (alongside the trait). Its `run_rubric`
  spawns the reviewer agent subprocess against the rendered review
  prompt and returns the agent's combined stdout; its
  `run_verifiers` (called only at `MintScope::Tree`) dispatches the
  deterministic verifier set + the integrity gate forward-resolution
  check and returns one `VerifierFailure` per failed dispatch
  outcome
  [test](production_mint_walker_exists_and_dispatches_rubric_and_verifiers)
- `run_gate_mint` in the loom CLI binary constructs the production
  walker and calls `mint::walk::walk(walker, scope, validator)` to
  obtain the `Vec<Finding>` it passes to `mint_findings_with_options`.
  No code path in `run_gate_mint` constructs `Vec::<Finding>::new()`
  unconditionally
  [test](run_gate_mint_dispatches_through_production_walker_not_empty_vec)
- `loom loop`'s per-bead path invokes `loom gate verify --bead <id>`
  followed by `loom gate mint --bead <id>` as subprocesses after the
  run-phase agent signals Success. Fix-up beads minted at this step
  become ready on the next outer-loop pass via `bd ready`
  [test](loop_per_bead_dispatches_verify_then_mint_after_run_phase_success)
- The production `exec_per_bead_gate` implementation actually spawns
  those subprocesses against `loom_bin` — argv shape `gate verify
  --bead <id> -s <spec>` then (only on verify exit 0) `gate mint
  --bead <id> -s <spec>`. The bullet above covers the runner-side
  routing on the typed `PerBeadGateOutcome`; this pins the
  subprocess seam itself so a regression that hard-codes an empty
  argv, reorders verify/mint, or drops the bead/spec flags is caught
  at the production controller, not just at the mock boundary
  [test](exec_per_bead_gate_invokes_loom_gate_verify_then_mint_subprocesses)

### Per-bead mint summary semantics

- `loom gate mint --bead <id>` exits 0 when `refused == 0 && errors
  == 0`, regardless of `minted`/`skipped` counts. The mint summary
  is printed to stdout unchanged
  [test?](mint_bead_scope_exits_zero_on_clean_summary)
- `loom gate mint --bead <id>` exits non-zero when `refused > 0` or
  `errors > 0`, and the summary header lists the non-zero counts so
  the loop's per-bead path can route on the typed exit
  [test?](mint_bead_scope_exits_nonzero_on_refused_or_errors)

Loop-side interpretation of these exit codes — routing `refused` to
`loom:blocked` with cause `mint-structural-violation`, threading
`errors` through the recovery loop — is owned by
[harness.md § Functional](harness.md#functional).

### Commands surface — bare gate and status

- Bare `loom gate` (no subcommand) prints `loom gate --help` —
  identical output to `loom gate --help`. No verifier runs, no cache
  read, no bd writes
  [test?](bare_loom_gate_prints_subcommand_help)
- `loom gate status` reads the sqlite status cache and prints the
  report per `Status cache` above; inherits the bare-invocation scope
  default (`--diff <molecule.base_commit>..HEAD` for active spec
  with an open epic, else `--diff HEAD`); `loom gate status --tree`
  reads the full workspace cache
  [test?](loom_gate_status_subcommand_reads_cache_with_default_scope)
- `loom gate status` is `refused_inside_loom() == false`; running
  under `LOOM_INSIDE=1` is allowed because the cache read is local
  and read-only
  [test?](loom_gate_status_is_allowed_under_loom_inside_env)

### Status cache

- Cache file is created on first `open` when the path is missing
  [test](open_creates_db_file_when_missing)
- A `CacheRow` round-trips through sqlite preserving every field
  [test](round_trip_through_sqlite_preserves_every_field)
- The `row_for` helper writes a row that round-trips through the cache
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

- `[check]` tier spawns one subprocess per annotation
  [test](dispatcher_spawns_one_subprocess_per_check_annotation)
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
- Workflow events (push, merge, bead lifecycle, fix-up bonding,
  molecule progress). Those are downstream of the gate's verdict, not
  properties the gate evaluates.
- The `loom:clarify` resolution channel itself — `loom msg` is the
  surface, defined in [harness.md](harness.md) Msg Modes.

# Loom Gate

The quality gate. Decides whether code is good enough to ship.

The umbrella concept covering all four stages (plan / per-diff /
push / standing safety net) and one command tree (`loom gate
<subcommand>`).
`loom gate verify` runs deterministic verifiers; `loom gate review`
runs the LLM rubric. Distinct from the *Verdict Gate* execution
layer in [harness.md](harness.md) — that section owns the
per-bead mechanics that wrap the gate; this spec owns the rubric,
the invariants, the lanes, and the stages.

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
  "keep this on top" path. Gate refuses merge; recovery is *fix the
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
| **`loom gate`** | Status | Reads cached results from the last `verify` / `review` / `audit` run (sqlite-backed) and prints a fast status report — no verifiers run. See *Status cache* for the hard latency target. |
| **`loom gate audit`** | All | Runs everything — `verify` then `review`. The full PR-gate path. |
| **`loom gate verify`** | Deterministic | Runs every `[check]` / `[test]` / `[system]` verifier. Cheap relative to review; expensive relative to status. Run frequently (pre-commit, on save, every CI commit). |
| **`loom gate check`** | Deterministic, one tier | Runs only `[check]`-tier verifiers (static analysis of source). Fastest tier; suitable for tight feedback loops. |
| **`loom gate test`** | Deterministic, one tier | Runs only `[test]`-tier verifiers, batched into one runner subprocess per invocation. |
| **`loom gate system`** | Deterministic, one tier | Runs only `[system]`-tier verifiers (containers, packaging, end-to-end). Slow; CI-only by default. |
| **`loom gate review`** | LLM judge | Runs the LLM rubric — conformance trace, contract closure, verifier honesty, mock discipline, invariant-clash scan. Expensive. Run selectively (bead completion, on demand, scheduled). Consumes `verify` results as input. Includes `[judge]`-tier criterion verifiers and the rubric walk over the diff. |
| **`loom gate judge`** | LLM judge, one lane | Runs only criterion-attached `[judge]` verifiers — skips the rubric walk. Useful when only one lane needs re-running. |
| **`loom gate rubric`** | LLM judge, one lane | Runs only the rubric walk over the diff — skips criterion-attached judges. |

### Scope flags

All gate subcommands take exactly one scope flag (mutually
exclusive), plus optional filters. The scope flag defines the
**input set** — the files the gate is being asked about. A verifier
runs iff its declared inputs intersect the input set (see *Verifier
inputs* below); otherwise it's skipped.

| Flag | Input set | Typical caller |
|---|---|---|
| `--bead <id>` | The bead's success-criteria input files + the bead's own diff | `loom run` per-bead verdict gate |
| `--diff <range>` | `git diff <range> --name-only` (committed + working tree in the range) | push gate (molecule.base_commit..HEAD); CI scoped to a PR; bare interactive `loom gate verify` |
| `--files <paths>` | Explicit path list | pre-commit hooks (`loom gate verify --files $(git diff --cached --name-only)`) |
| `--tree` | Every file in the workspace | nightly CI safety net; manual debugging; **not used by push gate** |

Filters compose with any scope flag:

- `--spec <label>` — narrow to one spec's criteria
- `<selector>` (positional) — run one specific verifier by its
  annotation target

**Default for bare invocation.** When no scope flag is passed, the
gate defaults to `--diff <molecule.base_commit>..HEAD` if
`current_molecule[<active_spec>]` is set (the "what would fail if I
pushed now?" question); else `--diff HEAD` (working-tree dirty
changes vs HEAD). Bare `loom gate audit` never silently expands to
`--tree` — users who want the full safety-net sweep type `--tree`
explicitly.

| Stage | Default invocation | Scope |
|---|---|---|
| Pre-commit hook | `loom gate verify --files $(git diff --cached --name-only)` | `--files` |
| `loom run` per-bead | `loom gate verify --bead <id>` | `--bead` |
| `loom run` molecule completion (push gate) | `loom gate audit --diff <molecule.base_commit>..HEAD` | `--diff` |
| Interactive bare `loom gate verify` | implicit `--diff <molecule.base_commit>..HEAD` if `current_molecule[<active_spec>]` is set; else `--diff HEAD` | `--diff` |
| Nightly CI / on-demand audit | `loom gate audit --tree` | `--tree` |

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
review`. For lane subsets without a named alias (e.g. "all
deterministic without `system`"), shell composition is the path:
`loom gate check && loom gate test`.

## Stages

Same gate, four points. Scope and cost-of-failure differ; the
underlying check is the same.

| Stage | Where | Scope | Cost-of-failure | Primary catches |
|---|---|---|---|---|
| **Plan** | `loom plan -n` / `loom plan -u` | Spec under interview | Lowest — no code yet | Missing claims, weak claims, missing verifier surfaces, invariant clashes in proposed spec changes |
| **Per-diff** | `loom gate verify --bead <id>` then `loom gate review --bead <id>` (or `loom gate audit --bead <id>` for both) | Spec sections the diff touches; the diff itself; tests in the diff | Medium — one bead's worth | Conformance gaps in diff, lint violations, weak verifiers, contract gaps inside one diff's reach, invariant clashes in proposed code changes |
| **Push** | `loom gate audit --diff <molecule.base_commit>..HEAD` (unconditionally on `loom run` molecule completion — see [harness.md FR1 + FR9](harness.md#functional)) | The molecule's own diff (files it touched) × every verifier whose declared inputs intersect that diff | Highest — **blocks push**, gate verdict encoded in [`GateOutcome`](harness.md#run-outcome-types) (`Success`/`Fail`/`NoGate`). `GateSuccess` is constructible only when all four FR9 conditions hold *and* on-disk review-log evidence is present; the type's `pub(crate)` constructor asserts each condition. Silent gate-skip is structurally unrepresentable | Conformance gaps in the molecule, integrity-gate findings (unresolved annotations, stub tests) within the molecule's diff, review concerns, dispatch errors |
| **Standing safety net** | `loom gate audit --tree` (on-demand, nightly CI, scheduled) | Entire spec tree × entire implementation | Catches **verifier-input-declaration drift** — any verifier the push-gate's `--diff` scope would have skipped on the same diff is surfaced here. Drift = a verifier-correctness bug, filed as `loom:clarify` against the verifier's owning bead | Cross-file incoherence the molecule's diff didn't surface, contracts orphaned across PRs, accumulated style/test regressions, template-vs-spec drift (Invariant 3), surface drift, verifier-input declarations that are too narrow |

The plan stage has no separate command invocation — the agent runs
the rubric inline during the planning interview, and `loom plan` is
the surface that opens that interview. The other two stages compose
`loom gate verify` and `loom gate review` (or invoke `loom gate
audit` for both) as listed.

The push stage is **non-optional and load-bearing across every
execution mode of `loom run`** — sequential, parallel, `--once`,
and `--all-specs`. It computes the four-condition AND of FR9 (bead
labels + verify exit + review exit + integrity findings) and refuses
push on any failure. The verdict is encoded in
[`GateOutcome`](harness.md#run-outcome-types): `Success` only
when all four conditions hold *and* the review wrote a non-empty
`review-*.jsonl` ending in a terminal `LOOM_COMPLETE` marker; `Fail`
on any failure with the reason explicit; `NoGate` only for legitimate
"no work to gate" terminals (`NoBeadsReady`, `OncePartial`). The
`GateSuccess` constructor is `pub(crate)` in the gate-invocation
module — no code path outside that module can mint one, so a clean
`loom run` exit without the gate actually firing is unrepresentable.
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

The per-diff stage composes `loom gate verify` and `loom gate review`
in sequence (or invokes `loom gate audit --bead <id>` for both).

**`loom gate verify --bead <id>`** runs all deterministic audits.
Marker parsing, `bd-closed` lookup, diff non-empty / empty, every
deterministic verifier (`[check]` / `[test]` / `[system]`) attached
to the bead's criteria (none short-circuits another), style linters
(`cargo clippy -- -D warnings`, `nix fmt --check`, `shellcheck`),
and any `[check]`-tier walks the consumer has registered for
cross-cutting structural audits.

Any `verify` failure routes into the existing hard-fail recovery
loop (`previous_failure` + `[loop] max_iterations`). Recovery doesn't
run `loom gate review` for the same iteration — mechanical failure
is sufficient grounds.

**`loom gate review --bead <id>`** runs the LLM rubric. Its inputs
are:

- the diff
- the bead's intent (title, description, success criteria)
- the *full* touched spec sections (not only the bullets the diff lines map to)
- the diffs and criteria of sibling beads bonded to the same molecule
- deterministic-verifier sources (`[check]` walk implementations, `[test]` function bodies, `[system]` scripts)
- `[judge]` rubric texts
- `loom gate verify` results from the immediately-prior run

Rubric checks. The **Concern token** column lists the value the
reviewer emits as the `LOOM_CONCERN: <token> -- <reasoning>` marker
payload when the check fails (per [harness.md § Verdict
Gate](harness.md#verdict-gate)). The `invariant-clash` token
routes to `loom:clarify` instead of recovery — see *Verdict* below.

| Check | Dimension | Lane | Concern token |
|---|---|---|---|
| **Conformance trace** — for every claim in touched spec sections, find a true code path (verifier-pass *or* LLM trace through current code). Scope includes the *full* touched spec sections — command-set tables, interface specs, decision tables, prose constraints — not only the bullets a diff line maps to. | Conformance | Hard fail | `spec-coherence-fail: <claim>` |
| **Contract closure** — for every multi-component contract the diff touches, verify completion in this diff or in bonded sibling diffs | Conformance | Hard fail | `orphan-integration: <contract>` |
| **Style-rule conformance** — diff complies with every rule in the consumer's pinned `{{ style_rules }}` document that linters cannot enforce mechanically. The judge discovers rule families from the document itself (no fixed prefix list — adapts to whatever convention the consuming project uses) and cites the rule id + file/line for each violation. | Style | Hard fail | `style-rule-violation: <rule-id>` |
| **Verifier honesty** — each deterministic verifier the diff adds or modifies (`[check]`, `[test]`, `[system]`) must support the claim it cites. Decomposed into four sub-checks; a verifier is honest iff it satisfies all four. (a) **verifier-bypass** — does the verifier actually exercise the live path? (b) **fabricated-result** — does the verifier's pass rely on a value the test itself synthesized? (c) **weak-assertion** — does the assertion meaningfully constrain the result, or does it tautologically pass? (d) **coincidental-pass** — does the verifier pass for the right reason, or because of an unrelated property of the system? The standing safety net re-checks existing verifiers against current spec/code to detect drift. | Test quality | Hard fail | `verifier-bypass: <verifier>` / `fabricated-result: <verifier>` / `weak-assertion: <verifier>` / `coincidental-pass: <verifier>` |
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

Verdict: any hard-fail concern → reviewer emits `LOOM_CONCERN:
<token> -- <reasoning>` → verdict gate routes to recovery loop with
cause `review-concern`. The `invariant-clash` concern is the
exception: it routes to `loom:clarify` on the affected bead with a
structured `## Options — …` block per the Options Format Contract.
The clarified bead is skipped by `bd ready` on subsequent ticks;
non-dependent beads in the molecule continue running. Push is held
until the clarify is resolved via `loom msg` (see push-gate
semantics in [harness.md](harness.md#functional)).

### Standing-safety-net checks

`loom gate verify --tree` and `loom gate review --tree` run
independently (or `loom gate audit --tree` for both); mechanical-only
is fast and frequent, full sweep is rarer.

`loom gate verify --tree` exercises every audit at tree scope: every
`[check]` / `[test]` / `[system]` verifier, all linters, all
`[check]`-tier walks the consumer has registered, walking every spec
and every implementation file.

`loom gate review --tree` runs the LLM rubric against the whole spec
set × implementation. The checks from the per-diff rubric apply,
scoped to the tree rather than a diff. Additional safety-net-only
check:

- **Template-vs-spec drift** (Invariant 3 enforcement). Reads every
  template loom uses (embedded in the loom binary, plus any
  consumer-provided overrides) against every spec in the consumer's
  spec tree. Flags any template instruction that contradicts a spec
  claim. Hard fail conceptually, but surfaced as a `bd` issue (no
  "merge to refuse" at the standing safety net). Concern token:
  `template-spec-drift`; the rubric body lives in the review prompt's
  *Template-vs-Spec Drift Walk* partial, gated on `--tree` scope.

Standing-safety-net flags become fix-up beads bonded to the spec's
**current molecule** as recorded in the state DB's `current_molecule`
table (see [harness.md SQLite State Store](harness.md#sqlite-state-store)).
The reviewer agent identifies each concern's owning spec from the
spec file the concern cites, looks up `current_molecule[<that_spec>]`
in the threaded map, and uses the resolved epic ID as `--parent` in
every `bd create`. At per-spec scope (`--tree --spec X`) the map has
one entry; at all-specs scope the map covers every spec with a
recorded pointer. Closed pointers are re-opened transparently (re-open is
unrestricted). When no entry exists for a spec the agent has concerns
about, behaviour depends on scope:

- **`loom gate audit --tree --spec <X>`** — refuses to start with a
  clear error naming the spec and pointing at `loom use <X> --epic
  <id>` (when an epic ID is known) or `loom todo --spec <X>` (to
  Tier-4-create a fresh molecule) as the fix. Returns non-zero, no
  agent spawn.
- **`loom gate audit --tree` (no `--spec`)** — for each concern about
  a spec missing from `current_molecule`, the reviewer agent creates
  a fresh recovery epic (`bd create --type=epic --title="<spec>
  recovery" --labels="spec:<spec>" --metadata
  "loom.base_commit=<HEAD>"`), bonds the fix-up to it, and the
  orchestrator writes the new epic to `current_molecule[<spec>]`
  post-run. Each auto-create surfaces on stdout naming the spec and
  new epic ID. Specs *not* missing from `current_molecule` use the
  recorded pointer (re-opening if closed) — the auto-create fires
  only when no pointer exists at all.

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

**Schema: `[runner.<tier>.<name>]` in `<workspace>/config.toml`.**
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
Consumers extend or override in `<workspace>/config.toml`. **Loom-
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
| `[test](name)` | Derived from test framework metadata. For Rust: walk `cargo metadata`, resolve the test's owning crate, declare the crate's source dirs. For pytest: pytest's collection output. For other frameworks: `<workspace>/config.toml` `[runner.<tier>] inputs_for_test = "<command>"`. |
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
evidence) and any non-zero exit as `pass=false` (stderr surfaced as
evidence). Verifiers that emit a JSON line are preferred — the
explicit evidence string clicks straight to the violation site — but
the exit-code fallback keeps simple presence/absence checks viable
without wrapping each one in a Rust walk.

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

2. **Stub-pointing — annotations whose verifier body invokes the
   `_pending_stub` sigil are flagged** (`StubTestFunction`). A stub
   means the criterion has no real evidence; the deterministic gate
   flags it without waiting for `loom gate review`'s
   verifier-honesty rubric.

3. **Atomic acceptance — each criterion carries exactly one
   annotation.** Two annotations on one criterion is a flag
   (ambiguous pass/fail when one passes and the other fails).
   N→1 sharing is allowed (multiple criteria pointing at the same
   verifier).

Failure output: `<spec>:<line>: annotation [tier](<target>) — does
not resolve` or `<spec>:<line>: criterion carries N annotations,
expected 1` or `<spec>:<line>: annotation [tier](<target>) points
at stub function`.

**Integrity findings are terminal at the push gate** (harness.md
FR9). `UnresolvedAnnotation` and `StubTestFunction` findings within
the molecule's diff scope refuse the push and apply `loom:clarify`
to the molecule's epic with an auto-generated `## Options — …` block
per the *Options Format Contract* above.

**Auto-generated options for `UnresolvedAnnotation`.** The gate has
enough information (target string, tier, spec location) to draft
options for the human:

- *Option 1* — Implement the missing verifier (walk / test / judge /
  system check) at the expected path.
- *Option 2* — Retarget the annotation to an existing verifier
  (gate lists nearest matches by name).
- *Option 3* — Remove the criterion at `<spec>:<line>` if it's
  superseded or out of scope.

**Auto-generated options for `StubTestFunction`.** Similar shape:

- *Option 1* — Implement the test body, replacing the
  `_pending_stub` sigil.
- *Option 2* — Retarget the annotation to a non-stub verifier.
- *Option 3* — Remove the criterion if the work isn't planned.

The integrity gate is itself a `[check]`-tier verifier (its own
spec criterion annotates back to its implementation), so every
`loom gate check` run includes a self-test of the gate's resolution
logic.

## Status cache

`loom gate` (no subcommand) reads from a sqlite-backed status cache
and prints a fast report. `loom gate verify`, `loom gate review`,
and the tier subcommands write to the cache as they run.

**Cache contents per criterion:**
- annotation target
- last-run timestamp and commit hash
- pass / fail / skipped (scope) verdict
- evidence string from the verifier's JSON output

**Cache schema** extends the existing state-db schema in
[harness.md](harness.md). One row per criterion, indexed
by `(spec_label, criterion_anchor)`.

**Report contents** when `loom gate` runs without subcommands:
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

- Hard-fail flags drive the existing recovery loop with
  `previous_failure` context.
- Invariant-clash flags raise `loom:clarify` beads per the Options
  Format Contract.
- Standing-stage flags create `bd` issues bonded to the relevant
  spec section.

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
  raise `loom:clarify` on the flagged bead; that bead is skipped
  by `bd ready` on subsequent ticks while non-dependent beads in
  the molecule continue running. `loom msg` resolves the clarify.
  Clashes never trigger fresh-agent retry.
- **Standing** — fix-up beads are bonded to the spec's
  `current_molecule` epic (state DB; closed pointer re-opened
  transparently). Multi-spec `--tree` sweeps with missing pointers
  auto-create recovery epics per *Standing-safety-net checks* above.
  Invariant clashes surface via `loom:clarify` in the next `loom
  msg` walk.

### Post-hoc recovery — when the push gate was skipped

**Use case.** A molecule's beads closed without `GateSuccess` being
constructed — e.g., a legacy run from before the type-shape
enforcement landed, or a manual `bd close` outside the gate. The
work shipped but was never audited; the codebase has unverified
divergence from the spec. The push-stage scope (`<molecule.base_commit>..HEAD`)
no longer applies because HEAD has moved on to subsequent
(also-unaudited) work, and the molecule's `loom.base_commit` would
include unrelated downstream commits.

**Canonical recovery path:** `loom gate audit --tree`, single-spec
or multi-spec. The standing-safety-net scope is exactly what's
needed — audit the full spec(s) against the full implementation,
no diff math, no dependence on a still-valid `loom.base_commit`.

```bash
# Single spec — current_molecule must be set first (the gate refuses
# at --tree --spec scope when the pointer is empty)
loom use <label> --epic <id>           # explicit seed; re-opens <id> if closed
                                       #   (or: loom init --rebuild seeds from
                                       #   the spec-index file)
loom gate audit --tree --spec <label>  # standing rubric over the spec × implementation
loom run                               # process the resulting fix-up beads; gate
                                       #   fires structurally on completion

# Across every spec in the workspace
loom init --rebuild                    # best-effort seed of current_molecule from
                                       #   the consumer's spec-index file
loom gate audit --tree                 # auto-creates recovery epics for specs
                                       #   missing from current_molecule
loom run --all-specs                   # iterates current_molecule, oldest-first
```

**Compositional safety.** The recovery flow's `loom run` produces
`GateOutcome` per molecule — silent skip is structurally
unrepresentable (see [harness.md Run Outcome Types](harness.md#run-outcome-types)).
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

### Standing-safety-net bonding

- `loom gate audit --tree --spec <X>` refuses to start with a
  non-zero exit when state.db has no `current_molecule[X]` entry;
  the error names the spec and points at `loom use <X> --epic <id>`
  or `loom todo --spec <X>` as the fix. No agent is spawned
  [test](tree_scope_refuses_when_no_current_molecule_for_spec)
- `loom gate audit --tree` (all-specs sweep) auto-creates a fresh
  recovery epic for each spec that has concerns but no
  `current_molecule` entry; bonds fix-ups to the new epic; writes
  the new pointer to state.db. Each auto-create surfaces on stdout
  naming the spec and new epic ID
  [test](tree_scope_auto_creates_epics_for_missing_current_molecule_specs)
- `loom gate audit --tree` (all-specs sweep) reuses existing
  `current_molecule` entries when set; bonds fix-ups to the
  recorded epic (re-opens transparently if closed); does NOT
  auto-create for specs that already have a pointer
  [test](tree_scope_reuses_existing_current_molecule_when_set)
- The reviewer agent prompt at `--tree` scope threads
  `current_molecule[<spec>]` (or the full map for all-specs scope)
  as the bonding target; `bd create --parent <epic>` writes wire
  through unchanged
  [test](tree_scope_threads_current_molecule_into_reviewer_prompt)

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

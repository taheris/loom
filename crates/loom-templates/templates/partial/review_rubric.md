## Verifier Honesty

The verdict-gate review's primary concern is **verifier honesty**: a
deterministic-tier annotation (`[check]`, `[test]`, or `[system]`) is
honest iff it satisfies all four sub-checks below. Walk each sub-check
against deterministic annotations affected by the changed live path, including
unchanged sibling criteria; additions or edits to annotations are not the only
way coverage can be lost. At `--tree` scope, re-walk every existing deterministic annotation against
current spec/code to catch drift. Failure on any sub-check is a hard
fail with the matching concern token.

**Sub-check 1 — `verifier-bypass`.** Does the verifier actually exercise
the live path? At least one deterministic-tier annotation on the bead
must hit the same binary, same argv shape, same env as the real
invocation. Bypass shapes to flag:

- The bead's full deterministic-tier annotation set is entirely mocks —
  no script runs the live path end-to-end.
- A test that asserts `result/bin/loom` exists instead of _running_ the
  binary at that path.
- A `cargo build` / `cargo check` standing in for a behavioural test on
  a module the diff never imports.

**Sub-check 2 — `fabricated-result`.** Does the verifier's pass rely on
a value the test itself synthesized? Fabrication shapes to flag:

- A test constructs the expected output, feeds it through a thin
  identity wrapper, and asserts the wrapper returned it unchanged.
- A test stubs the system-under-test to return the answer the assertion
  then checks.
- A round-trip test whose serializer and deserializer are both mocks of
  the production codecs.

**Sub-check 3 — `weak-assertion`.** Does the assertion meaningfully
constrain the result, or does it tautologically pass? Weak-assertion
shapes to flag:

- `assert!(result.is_some() || result.is_none())`, `assert_eq!(x, x)`,
  or any predicate that holds for every possible value of the variable.
- A test whose only assertion is "no panic" on a code path that never
  panics.
- An assertion that the call returned (without checking _what_ it
  returned) on a function whose only error case is unreachable in the
  test fixture.

**Sub-check 4 — `coincidental-pass`.** Does the test pass for the right
reason, or because of an unrelated property of the system?
Coincidental-pass shapes to flag:

- A fixture that diverges from the live invocation derivation
  (different env vars, different argv, different working directory) so
  the assertion holds for a fixture reason, not a behaviour reason.
- A test ending with `|| true`, silent `2>/dev/null`, or any swallowed
  exit code that lets the script return 0 regardless of the real
  outcome.
- A test that passes today only because a dispatcher silently routes
  every call to a default branch — change the dispatcher and the test
  no longer means what it claims to mean.

A test that satisfies all four sub-checks is honest. If a sub-check
fails, emit one finding line per failing sub-check (`verifier-bypass`,
`fabricated-result`, `weak-assertion`, `coincidental-pass` in the
`token` field), citing the offending test path in `target` and the
spec claim it purports to verify in `evidence`. The walk's terminator
is `LOOM_CONCERN` carrying a one-sentence summary; per-finding routing
is decided by the driver from each finding's `token`, not by the
terminal marker. See _Findings — Streaming Wire Format_ in the prompt
body for the full wire-format contract.

## Mock Discipline

Mocks are not forbidden. Each mock needs a discernible reason: cost,
flakiness, isolating an orthogonal concern, driving a hard-to-trigger
error path. A mock standing in for the very thing the test claims to
test flags `mock-discipline`.

**Acceptable mocks (no flag):**

- Mocking the LLM API in a retry-behaviour test — real calls are slow
  and flaky, and the test's concern is the retry logic.
- Mocking the filesystem when the test's actual concern is argument
  parsing or config resolution.
- Mocking a third-party service to drive an error path that's hard to
  trigger live.

**Flagged mocks:**

- Mocking the agent backend in a test that claims to test agent
  integration — the mock IS the thing under test.
- Mocking the database in an integration test where the test's stated
  scope includes schema or migration behaviour.

## Coverage Reductions

Compare coverage before and after the reviewed change against current contracts,
including relevant unchanged sibling owners. Trace the subjects and obligations
that disappeared, not just the tests that remain green:

- Inspect removed tests, verifier inputs, discovery membership, role exemptions,
  skips, narrowed checker responsibilities, and policy defaults. Which subjects
  or obligations no longer receive checks? Passing remaining checks does not
  justify checking less.
- An obligation may move to another owner, but require a demonstrated replacement:
  inspect its verifier and evidence that it exercises the same required behavior
  on the affected subjects. A replacement claim, owner name, or renamed test is
  not proof. Otherwise require an authorized spec change justifying the loss.
- Accept dependency-precision changes that retain the same subjects, obligations,
  and meaningful coverage. Removing README.md from a code-only verifier's inputs
  can be precision; removing it from a checker for README/spec-index agreement
  loses an obligation. Judge actual responsibility, not filename or keyword counts.
- An unchanged billing contract can require invoice rounding even when a diff
  removes only a shared arithmetic test or excludes billing from discovery.
  Trace that sibling obligation before declaring the reduction safe.

Use ordinary findings: `verifier-too-narrow` or `spec-coherence-fail` with a
`Criterion` target naming the affected owner's existing anchor, or the applicable
verifier-honesty token with an `Annotation` target. Include all affected owners
in `bonds` and explain the lost coverage or unproven replacement in `evidence`.
An unresolved invariant decision uses `route="clarify"` with canonical Options
evidence; otherwise use the existing blocking/deferred routes for the dispatch
scope. Finish a walk with findings using `LOOM_CONCERN`. Do not introduce a new
concern token, separate gate, or mandatory human signoff for optimizations.

## Invariant-Clash Anchors

When scanning each touched spec section for load-bearing invariants the
diff may contradict, use spec conventions as anchors:

- **`## Out of Scope` sections** — items here are explicit non-goals; a
  diff that implements one is a clash.
- **`## Non-Functional` sections / NFR-prefixed items** — performance,
  isolation, security, portability commitments. A diff that breaks any
  is a clash.
- **Imperative-keyword sentences** — prose using `MUST`, `MUST NOT`,
  `NEVER`, `ALWAYS`, "is the single source of truth", "is the only
  caller". Each such sentence is an invariant statement; check the diff
  against it.
- **Architectural claims** — phrasings like _"X never calls Y"_, _"Z is
  reconstructable from the on-disk state"_, _"the binary has no `foo`
  subcommand"_. A diff that violates the claim is a clash.
- **Schema / data-structure declarations** — type signatures, table
  definitions, JSON shapes, decision-table rows embedded in the spec. A
  diff that changes the shape silently is a clash.

These are anchors, not an exhaustive checklist. Also catch **prose-only
invariants** that lack a structural anchor — a paragraph in the body of
a section can carry an invariant claim without an `## Out of Scope`
heading or a `MUST` keyword. When uncertain whether a section is
load-bearing, treat it as one and ask; false positives are cheaper than
misses.

## Surface Conformance (deterministic, not LLM-judged)

Command / flag / grouping / removed-surface drift is **not** an LLM
rubric dimension — `loom gate verify` is the deterministic audit (see
FR13 in `specs/harness.md` and _Surface-conformance audit_ in
`specs/gate.md`). Do not duplicate it in the LLM walk.

Surface failures produced by `loom gate verify` are still part of this
review's input set. Treat them as deterministic gate evidence: cite them
in prose when they explain a conformance problem, but do **not** emit a
separate LLM concern token for surface drift. If the diff itself creates
a spec/code mismatch beyond the deterministic failure, use
`spec-coherence-fail` with a `Criterion` target.

## Cross-Spec Walk (`--tree` scope only)

When this review runs at `--tree` scope, walk every spec under `specs/`
and flag contradictions **between** specs. Per the _Single source of
truth_ rule in `docs/spec-conventions.md`:

- If a fact appears in two specs identically, one is wrong (drift
  incoming) — flag `cross-spec-clash` and name the spec that should
  cross-reference the other.
- If two specs state the same fact in different words, one is
  paraphrasing — flag `cross-spec-clash` and name the spec to be
  rewritten as a cross-reference.
- If two specs disagree on a fact, the contradiction is a flag.

At `--bead` or `--diff` scope an exhaustive standing cross-spec walk is
out of scope. Current sibling contracts relevant to changed subjects,
obligations, shared seams or replacements remain in view even when unchanged
or not bead-bonded. Broaden uncertain relevance; a discrepancy noticed there
falls under the conformance or invariant-clash walk above.

For each clash, emit a finding line with `token = "cross-spec-clash"`,
a `Criterion` target naming the primary spec/anchor, every involved spec
in `bonds`, and the other side(s) quoted in `evidence`.

## Template-vs-Spec Drift Walk (`--tree` scope only)

When this review runs at `--tree` scope, walk every prompt template loom
ships (every file under `crates/loom-templates/templates/` — the embedded
template set, including partials) against every spec under `specs/`. The
check enforces Invariant 3 from `specs/gate.md`: a template that
directs agents toward behaviour the spec contradicts produces cascading
damage as the agent follows the template literally instead of the
contract.

For each template, judge whether any instruction it gives an agent
contradicts a claim in the spec set:

- An instruction to emit a marker / token / label the spec does not
  define, or that the spec defines with a different meaning.
- A workflow step that violates an explicit imperative-keyword sentence
  (`MUST`, `MUST NOT`, `NEVER`) in a spec section the template
  references.
- A command shape the template tells the agent to run that contradicts
  the spec's command surface (e.g. flag spelled differently, subcommand
  the spec marks removed).
- A persistence path (`bd update --notes`, `bd update --add-label=...`)
  the template prescribes that disagrees with the persistence contract
  the spec sets.

A template instruction that the spec set does not speak to is not
drift — only contradictions count. When uncertain whether a template
phrasing conflicts with a spec claim, treat it as drift and flag; false
positives surface as `bd` discussion, misses ship broken templates.

At `--bead` or `--diff` scope this walk is out of scope; per-diff
template edits are reviewed against current relevant contracts, including
unchanged siblings, under the conformance and invariant-clash walks above.

For each drift, emit a finding line with `token = "template-spec-drift"`,
the offending template path in `target`, and the spec section it
contradicts in `evidence`. Terminate the walk with `LOOM_CONCERN`
carrying a one-sentence summary per the wire-format contract in
_Findings — Streaming Wire Format_.

## Spec-Conventions Walk (`--tree` scope, and spec edits)

When this review runs at `--tree` scope, walk every spec under `specs/`
against `docs/spec-conventions.md`; the standing safety net must catch
spec-convention drift even when no diff edited the spec. At `--diff` /
`--bead` scope, run this walk only when the diff edits spec markdown and
limit it to the touched spec sections. In both cases, walk the
convention's _In scope_ / _Out of scope_ / _Section structure_ /
_Trust tiers_ / _Single source of truth_ / _Length guidance_ sections:

- Status checkboxes (`[ ]` / `[x]`) inside Success Criteria → flag.
- `## Affected Files` listing an in-flight change → flag (the
  convention permits the section only when it enumerates files the spec
  _owns_ as source of truth).
- `## Implementation Notes` / `## Decisions Log` / `## Changelog` /
  `## History` sections in the spec body → flag.
- Internal file paths, line numbers, or module-layout claims with no
  architectural role → flag.
- A `[check]`, `[test]`, or `[system]` annotation on a claim that requires
  judgement (mock discipline, scope, prose style rule) — tier-skip → flag.
- A criterion bullet with no annotation, or whose annotation points at
  a missing or stubbed verifier → already a `loom gate verify` flag;
  surface it here too when introduced by the diff or visible in the
  tree-scope sweep.

For each violation, emit a finding line with
`token = "spec-conventions-violation"`, a `Criterion` target naming the
offending spec/anchor, and the convention section by name in `evidence`.
Terminate the walk with `LOOM_CONCERN` carrying a one-sentence summary
per the wire-format contract in _Findings — Streaming Wire Format_.

## Style-Rule Conformance

The diff must satisfy every applicable rule in `{{ style_rules }}`. This
is the load-bearing defense for any rule that linters cannot mechanically
enforce — most rules in the document are prose, and the LLM judge is what
enforces them. _"Style looks fine"_ is not an acceptable answer; the
output must enumerate which rules were checked.

**How to walk the document.** Open `{{ style_rules }}` and walk every rule
family the document defines, in order, rule by rule. Discover the families
from the document itself; do not assume a fixed prefix list.

For each rule, judge whether the diff satisfies it. A rule that does not
apply to this diff (for instance, a shell-family rule against a pure-Rust
diff) is _checked and dismissed_, not skipped silently — say so in the
output.

**Citation contract.** For every violation you identify, the output
**must** cite both:

- the **rule id** — e.g. `<FAMILY>-<N>`, using the family prefix and number
  exactly as they appear in `{{ style_rules }}`
- the **offending file and line range** — e.g.
  `crates/loom-driver/src/agent/parser.rs:142-156`

One violation per bullet; never aggregate multiple rules into one
citation. A finding without a rule id is not actionable; a finding
without a file/line range is not auditable.

**Flag emission.** Any style-rule violation is a hard fail. For each
violation, emit a finding line with `token = "style-rule-violation"`,
the offending file and line range in `target`, and the rule id in
`evidence`. Terminate the walk with `LOOM_CONCERN` carrying a
one-sentence summary that names the most load-bearing violation by
rule id, per the wire-format contract in _Findings — Streaming Wire
Format_; per-violation citations above carry the full list in the
visible body of your response.

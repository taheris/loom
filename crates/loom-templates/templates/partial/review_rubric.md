## Verifier Honesty

The verdict-gate review's primary concern is **verifier honesty**: a
`[verify]` test is honest iff it satisfies all four sub-checks below.
Walk each sub-check against every `[verify]` the diff adds or modifies;
at `--tree` scope, re-walk every existing `[verify]` against current
spec/code to catch drift. Failure on any sub-check is a hard fail with
the matching concern token.

**Sub-check 1 — `verifier-bypass`.** Does the verifier actually exercise
the live path? At least one `[verify]` on the bead must hit the same
binary, same argv shape, same env as the real invocation. Bypass shapes
to flag:

- The bead's full `[verify]` set is entirely mocks — no script runs the
  live path end-to-end.
- A test that asserts `result/bin/loom` exists instead of *running* the
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
- An assertion that the call returned (without checking *what* it
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
fails, name the matching concern token (`verifier-bypass`,
`fabricated-result`, `weak-assertion`, `coincidental-pass`) in the prose
body and cite the offending test path and the spec claim it purports to
verify. When you finalize the phase, emit a single `LOOM_CONCERN: <token>
-- <reason>` line on the final line of the response for the strongest
sub-check failure; per the Flag Emission Schema, only one structured
concern reaches the verdict gate.

## Mock Discipline

Mocks are not forbidden. Each mock needs a discernible reason: cost,
flakiness, isolating an orthogonal concern, driving a hard-to-trigger
error path. A mock standing in for the very thing the test claims to
test flags `mock`.

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
- **Architectural claims** — phrasings like *"X never calls Y"*, *"Z is
  reconstructable from the on-disk state"*, *"the binary has no `foo`
  subcommand"*. A diff that violates the claim is a clash.
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
rubric dimension — `loom check surface` is the deterministic audit (see
FR13 in `specs/harness.md` and *Surface-conformance audit* in
`specs/gate.md`). Do not duplicate it in the LLM walk.

Surface-drift findings produced by `loom check surface` are still part
of this review's input set. If a surface failure appears in the gate
inputs alongside the diff, surface it as `surface-drift` in the flag
emission with the offending command / flag named in the detail.

## Cross-Spec Walk (`--tree` scope only)

When this review runs at `--tree` scope, walk every spec under `specs/`
and flag contradictions **between** specs. Per the *Single source of
truth* rule in `docs/spec-conventions.md`:

- If a fact appears in two specs identically, one is wrong (drift
  incoming) — flag `cross-spec-clash` and name the spec that should
  cross-reference the other.
- If two specs state the same fact in different words, one is
  paraphrasing — flag `cross-spec-clash` and name the spec to be
  rewritten as a cross-reference.
- If two specs disagree on a fact, the contradiction is a flag.

At `--bead` or `--diff` scope this walk is out of scope; only the
touched spec section + bonded sibling specs are in view, and any
cross-spec discrepancy noticed there falls under the per-section
invariant-clash walk above.

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
template edits are reviewed against the spec sections the diff itself
touches, under the conformance-trace and invariant-clash walks above.

When you finalize the phase, emit `LOOM_CONCERN: template-spec-drift
-- <summary>` on the final line for the strongest contradiction. The
visible body cites the template path and the spec section it
contradicts for every drift found.

## Spec-Conventions Walk (for spec edits)

When the diff edits spec markdown under `specs/` (not just code), the
touched spec sections **must** comply with `docs/spec-conventions.md`.
Walk the convention's *In scope* / *Out of scope* / *Section structure*
/ *Trust tiers* / *Single source of truth* / *Length guidance* sections
against each edited spec section:

- Status checkboxes (`[ ]` / `[x]`) inside Success Criteria → flag.
- `## Affected Files` listing an in-flight change → flag (the
  convention permits the section only when it enumerates files the spec
  *owns* as source of truth).
- `## Implementation Notes` / `## Decisions Log` / `## Changelog` /
  `## History` sections in the spec body → flag.
- Internal file paths, line numbers, or module-layout claims with no
  architectural role → flag.
- A `[verify]` annotation on a claim that requires judgement (mock
  discipline, scope, prose style rule) — tier-skip → flag.
- A criterion bullet with no annotation, or whose annotation points at
  a missing or stubbed verifier → already a `loom check criteria` flag;
  surface it here too if the edit introduces it.

When you finalize the phase, emit `LOOM_CONCERN: spec-conventions-violation
-- <summary>` on the final line for the strongest violation. The visible
body cites the convention section by name and the offending spec file/line
range for every violation found.

## Style-Rule Conformance

The diff must satisfy every applicable rule in `{{ style_rules }}`. This
is the load-bearing defense for any rule that linters cannot mechanically
enforce — most rules in the document are prose, and the LLM judge is what
enforces them. *"Style looks fine"* is not an acceptable answer; the
output must enumerate which rules were checked.

**How to walk the document.** Open `{{ style_rules }}` and walk every rule
family the document defines, in order, rule by rule. Discover the families
from the document itself; do not assume a fixed prefix list.

For each rule, judge whether the diff satisfies it. A rule that does not
apply to this diff (for instance, a shell-family rule against a pure-Rust
diff) is *checked and dismissed*, not skipped silently — say so in the
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

**Flag emission.** Any style-rule violation is a hard fail. The final
line of the response must be a single `LOOM_CONCERN: style-rule --
<summary>` per the Flag Emission Schema below. The summary should name
the most load-bearing violation by rule id; the per-violation citations
above carry the full list in the visible body of your response.

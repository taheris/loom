## Findings — Streaming Wire Format

Communicate every concern as a `LOOM_FINDING` record in normal assistant text,
streamed as findings are identified (not batched at end-of-walk). Review is
**inspection-only**: never invoke `bd create`, `bd update`, or `bd mol bond`.
The trusted driver records/materializes admitted findings at its act boundary;
decoding a record is not permission to mutate Beads or publish.

### Emit shape

```text
LOOM_FINDING: {"token":"<token>","route":"blocking|deferred|clarify","bonds":["<spec>"],"target":<target>,"evidence":"<evidence>"}
```

- **`token`** — rubric concern from the compiled Findings metadata table below.
- **`route`** — `blocking` requests current-bead recovery, `deferred` requests
  remediation outside its hot path, and `clarify` carries human-decision
  Options evidence. At tree scope prefer deferred or clarify; blocking is
  accepted as ready remediation for compatibility. Push-gate scope rejects
  blocking. Neither a route nor a terminal authorizes reviewer Beads writes.
- **`bonds`** — nonempty array of actual workspace spec labels; the driver
  selects the bonding lead.
- **`target`** — tagged object; its `kind` and identity fields must match the
  token, resolve in the workspace and obey the active dispatch scope.
- **`evidence`** — reasoning stored verbatim with the finding. Every
  `route="clarify"` finding **MUST** embed one canonical Options brief here.
  Do not duplicate the evidence in adjacent prose or reproduce briefs on a
  source/epic. Ordinary commentary before the terminal is otherwise allowed.

Compact JSON is convenient, but pretty-printed objects are equally valid.
Escape newlines, quotes and controls inside evidence strings: raw JSON-string
newlines are **not repaired**. No preceding prose supplies fields or questions.
Emit live messages in column one, without fences; the fenced examples here are
instructional data, not live agent output. Malformed recognized records fail
with typed diagnostics retaining raw text, source spans and valid context;
there is no silent skip or guessed boundary in an unterminated string.

### Canonical target kinds and scopes

This table is compiled from Findings' prompt-visible rubric subset. Driver-only
deterministic/integrity tokens are not reviewer emit instructions. Target fields
are illustrated by the executable examples below.

| Token | Target kind | Scope |
| ----- | ----------- | ----- |
{% for token in self.finding_tokens() -%}
| `{{ token.as_wire() }}` | `{{ token.expected_target_kind().as_wire() }}` | `{{ token.scope_kind().label() }}` |
{% endfor %}

### Validation rules

- `Criterion.anchor` must be copyable from the spec: a heading slug, shown
  criterion ID, attached verifier/judge function name or target string.
- `Annotation.target_string` is only the raw declared annotation target, without
  a spec/line prefix or `[tier]` wrapper.
- `target.spec` must appear in `bonds` for Criterion and Invariant targets.
  Invariant `section` names an actual heading; `tag` is a short slug made from
  words in its prose, not an invented label.
- StyleRule includes both `rule_id` and a concrete, stable `subject`; a rule or
  bare line number alone is too broad for identity/suppression.
- File-shaped TestPath, LockSite and Template targets must exist. LockSite uses
  a source line. Unresolved labels/anchors/files, wrong token/target
  combinations and forbidden scopes fail contextual resolution.
- Clarify evidence must carry the Options Format Contract. Missing or malformed
  briefs are downgraded by materialization to `loom:blocked` with cause
  `clarify-without-options`, not repaired from adjacent narration.

### Concern tokens

Verifier-honesty tokens identify bypassed live paths, tautological assertions,
fabricated pass results or coincidental passes; their targets are annotations.
Mock discipline identifies tests substituting internal logic; verifier-too-narrow
identifies a criterion checked on only one side of its seam. Concurrency findings
identify synchronization sites missing contention tests. Judge flags identify
unsatisfied rubric criteria. Style-rule evidence cites the violating rule and
concrete file/range; coherence evidence identifies unrealized spec claims.
Orphan integration identifies an incomplete cross-component contract. Scope
creep/shortfall compare a finite diff with the task's criterion obligations.
Tree-only template drift, cross-spec clash and spec-convention violations cite
respectively a template, the primary spec/anchor with all involved bonds, or the
offending spec/anchor and convention section.

- **Invariant clash and all other clarify-worthy findings** carry contextual
  options (typically 2–4, each with a concrete cost), not a fixed menu:

  {% filter indent(2) %}{% include "partial/options_format.md" %}{% endfilter %}

  Embed this Markdown as escaped JSON evidence, once per finding. The trusted
  driver lifts the brief into the decision description; the reviewer does not.

### Executable examples

These sample workspace declarations use spec `example`, heading `Acceptance`,
contract `example-contract`, an annotated Rust test and an existing template.
Substitute actual workspace identities and concrete evidence, not invented
anchors. Each block is a separate completed review, not messages to concatenate.

Any-scope rubric examples (also valid at tree and push-gate scope):

```text loom-example any-scope
LOOM_FINDING: {"token":"spec-coherence-fail","route":"deferred","bonds":["example"],"target":{"kind":"Criterion","spec":"example","anchor":"acceptance"},"evidence":"The declared live-path behavior is absent."}
LOOM_FINDING: {"token":"orphan-integration","route":"deferred","bonds":["example"],"target":{"kind":"Contract","id":"example-contract"},"evidence":"The producer and consumer contract do not join."}
LOOM_FINDING: {"token":"style-rule-violation","route":"deferred","bonds":["example"],"target":{"kind":"StyleRule","rule_id":"RS-12","subject":"tests/example.rs#run"},"evidence":"The production value relies on a sentinel; RS-12 forbids it."}
LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["example"],"target":{"kind":"Annotation","target_string":"crate::example::test_live"},"evidence":"The annotation bypasses the real consumer."}
LOOM_FINDING: {"token":"weak-assertion","route":"deferred","bonds":["example"],"target":{"kind":"Annotation","target_string":"crate::example::test_live"},"evidence":"The assertion is tautological."}
LOOM_FINDING: {"token":"fabricated-result","route":"deferred","bonds":["example"],"target":{"kind":"Annotation","target_string":"crate::example::test_live"},"evidence":"The test synthesizes its own success."}
LOOM_FINDING: {"token":"coincidental-pass","route":"deferred","bonds":["example"],"target":{"kind":"Annotation","target_string":"crate::example::test_live"},"evidence":"The asserted path was never called."}
LOOM_FINDING: {"token":"mock-discipline","route":"deferred","bonds":["example"],"target":{"kind":"TestPath","path":"tests/example.rs"},"evidence":"The test mocks the internal logic it claims to check."}
LOOM_FINDING: {"token":"verifier-too-narrow","route":"deferred","bonds":["example"],"target":{"kind":"Criterion","spec":"example","anchor":"acceptance"},"evidence":"Only the producer side is exercised."}
LOOM_FINDING: {"token":"concurrency-untested","route":"deferred","bonds":["example"],"target":{"kind":"LockSite","file":"tests/example.rs","line":1},"evidence":"The introduced synchronization has no contention test."}
LOOM_FINDING: {"token":"judge-flag","route":"deferred","bonds":["example"],"target":{"kind":"Criterion","spec":"example","anchor":"acceptance"},"evidence":"The criterion's rubric is unsatisfied."}
LOOM_FINDING: {
  "token":"invariant-clash","route":"clarify","bonds":["example"],
  "target":{"kind":"Invariant","spec":"example","section":"Invariant","tag":"single-writer"},
  "evidence":"The change breaks the single writer constraint. Marker-looking text LOOM_COMPLETE is evidence, not a terminal.\n\n## Options — Choose writer ownership\n\n### Option 1 — Keep one writer\nRework the consumer; cost: churn.\n\n### Option 2 — Coordinate multiple writers\nAdd coordination; cost: coupling and contention risk."
}
LOOM_CONCERN: {"summary":"The contract and its verification have gaps."}
```

Finite per-bead examples only:

```text loom-example per-bead
LOOM_FINDING: {"token":"scope-creep","route":"blocking","bonds":["example"],"target":{"kind":"Criterion","spec":"example","anchor":"acceptance"},"evidence":"The diff changes unrelated behavior outside task acceptance."}
LOOM_FINDING: {"token":"scope-shortfall","route":"deferred","bonds":["example"],"target":{"kind":"Criterion","spec":"example","anchor":"acceptance"},"evidence":"Required behavior is missing from the diff."}
LOOM_CONCERN: {"summary":"The finite deliverable does not match its acceptance scope."}
```

Standing tree examples only:

```text loom-example tree
LOOM_FINDING: {"token":"template-spec-drift","route":"deferred","bonds":["example"],"target":{"kind":"Template","path":"crates/loom-templates/templates/example.md"},"evidence":"The prompt contradicts the contract."}
LOOM_FINDING: {"token":"cross-spec-clash","route":"deferred","bonds":["example","sibling"],"target":{"kind":"Criterion","spec":"example","anchor":"acceptance"},"evidence":"The sibling contract contradicts this acceptance claim."}
LOOM_FINDING: {"token":"spec-conventions-violation","route":"deferred","bonds":["example"],"target":{"kind":"Criterion","spec":"example","anchor":"acceptance"},"evidence":"The contract violates the authoring convention."}
LOOM_CONCERN: {"summary":"Standing contracts and prompts disagree."}
```

### Streaming + terminator pairing rule

A completed inspection pairs zero findings with Complete, or one or more
findings with Concern and a nonblank JSON `summary`; never emit both. These
terminals are mutually exclusive. The summary is a short verdict-log digest,
not repeated per-finding evidence or routing authority. A valid pretty terminal:

```text loom-example
LOOM_FINDING: {"token":"judge-flag","route":"deferred","bonds":["example"],"target":{"kind":"Criterion","spec":"example","anchor":"acceptance"},"evidence":"The live consumer is not tested."}
LOOM_CONCERN: {
  "summary": "The criterion lacks live consumer coverage."
}
```

| Findings | Terminal                                            | Result                                                 |
| -------- | --------------------------------------------------- | ------------------------------------------------------ |
| 0        | Complete                                            | Clean inspection                                       |
| ≥1       | Concern with nonblank summary                       | Findings retained for trusted routing                  |
| 0        | Concern                                             | ConcernWithoutFindings recovery                        |
| ≥1       | Complete                                            | FindingsWithoutConcern recovery with findings retained |
| any      | Malformed, missing, duplicate or misplaced terminal | Typed recovery; no manufactured success                |

Retry/Blocked are cannot-complete outcomes, documented below, not clean
inspection. Missing/duplicate terminals or a record/commentary after a terminal
fail shared decoding. Valid records retained on failure are diagnostic context,
not permission to mint. Concern is review-only; Clarify is not a review record.

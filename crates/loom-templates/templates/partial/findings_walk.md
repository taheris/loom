## Findings — Streaming Wire Format

You communicate every concern by emitting one `LOOM_FINDING:` JSON
line per finding on stdout, **streamed as findings are identified**
(not batched at end-of-walk). The driver parses each line
incrementally and mints the corresponding fix-up beads itself — the
agent never invokes `bd create` / `bd update` / `bd mol bond`.

### Emit shape

```text
LOOM_FINDING: {"token":"<token>","route":"blocking|deferred|clarify","bonds":["<spec>",...],"target":<target>,"evidence":"<evidence>"}
```

- **`token`** — concern identifier from the closed-set enum listed
  under *Concern tokens* below.
- **`route`** — workflow route for this finding: `blocking` retries
  the current bead, `deferred` records remediation outside the current
  bead's hot path, and `clarify` creates human-decision work with an
  options block.
- **`bonds`** — array of spec labels the fix-up should bond to.
  Always present, always at least one element. The driver picks the
  bonding lead from this array.
- **`target`** — tagged JSON object whose `kind` discriminator selects
  the variant per the table below; carries identity-bearing fields
  specific to the variant.
- **`evidence`** — your reasoning string, stored verbatim on the
  minted fix-up bead's description. For `invariant-clash` findings,
  this **MUST** embed the canonical `## Options — …` block per the
  Options Format Contract.

One JSON object per line. Do not pretty-print across multiple lines —
the driver parses one line at a time.

### Canonical target shapes per token

| Token | `target` shape |
|---|---|
| `spec-coherence-fail` | `{"kind":"Criterion","spec":"<spec>","anchor":"<anchor>"}` |
| `orphan-integration` | `{"kind":"Contract","id":"<contract-id>"}` |
| `style-rule-violation` | `{"kind":"StyleRule","rule_id":"<rule-id>","subject":"<stable-subject>"}` |
| `verifier-bypass` | `{"kind":"Annotation","target_string":"<target>"}` |
| `weak-assertion` | `{"kind":"Annotation","target_string":"<target>"}` |
| `fabricated-result` | `{"kind":"Annotation","target_string":"<target>"}` |
| `coincidental-pass` | `{"kind":"Annotation","target_string":"<target>"}` |
| `mock-discipline` | `{"kind":"TestPath","path":"<path>"}` |
| `verifier-too-narrow` | `{"kind":"Criterion","spec":"<spec>","anchor":"<anchor>"}` |
| `concurrency-untested` | `{"kind":"LockSite","file":"<file>","line":<line>}` |
| `judge-flag` | `{"kind":"Criterion","spec":"<spec>","anchor":"<anchor>"}` |
| `invariant-clash` | `{"kind":"Invariant","spec":"<spec>","section":"<section>","tag":"<tag>"}` |
| `template-spec-drift` | `{"kind":"Template","path":"<path>"}` — `--tree` scope only |

`scope-creep` and `scope-shortfall` are per-bead-only tokens; do not
emit them at `--tree` scope. `template-spec-drift`, `cross-spec-clash`,
and `spec-conventions-violation` apply at `--tree` scope only (see
`specs/gate.md` § *Standing-safety-net checks*).

Example lines:

```text
LOOM_FINDING: {"token":"spec-coherence-fail","route":"blocking","bonds":["gate"],"target":{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"},"evidence":"The bead claims to verify live-path coverage but every annotation mocks the binary."}
LOOM_FINDING: {"token":"style-rule-violation","route":"deferred","bonds":["gate"],"target":{"kind":"StyleRule","rule_id":"RS-12","subject":"crates/loom-gate/src/finding.rs#Finding"},"evidence":"crates/loom-gate/src/finding.rs:42-58 holds a placeholder String that consumers must overwrite — RS-12 forbids placeholder fields on production types."}
LOOM_FINDING: {"token":"concurrency-untested","route":"deferred","bonds":["harness"],"target":{"kind":"LockSite","file":"crates/loom-workflow/src/run/runner.rs","line":210},"evidence":"New Arc<Mutex<T>> introduced at runner.rs:210 has no concurrent-load test exercising contention."}
```

### Validation rules

- **`target.spec` MUST appear in `bonds`** for `Criterion` and
  `Invariant` target variants. You cannot cite a criterion or
  invariant in spec X while bonding only to spec Y. The driver
  rejects a violating finding with a typed parse error and fails the
  mint invocation. This rule applies to every token whose canonical
  target is `Criterion` (`spec-coherence-fail`,
  `verifier-too-narrow`, `judge-flag`) and the `Invariant` target
  (`invariant-clash`).
- **`StyleRule` targets MUST include a concrete `subject`** in
  addition to `rule_id`; a rule-only target is too broad for dedup or
  suppression, and a bare line number is not a stable subject.
- **`invariant-clash` findings MUST embed the canonical `## Options — …`
  block in their `evidence` field**. The driver lifts the block into
  the minted clarify bead's description; if it is missing, `loom
  msg`'s queue stays empty even though the finding minted a bead.
- **Malformed lines fail the run.** A `LOOM_FINDING:` line that does
  not parse — invalid JSON, unknown token, a `bonds` element that
  does not resolve to a workspace spec, a `target` variant
  mismatching the token's expected variant, or unresolved target
  content (criterion anchor not in spec, file path absent on disk) —
  is rejected with a typed error naming the offending line. No
  silent skip.

### Concern tokens

`<token>` is one of the following enum tokens (lowercase, hyphenated).
The first four are the verifier-honesty sub-checks — one finding per
failing sub-check, cited against the offending test path:

- `verifier-bypass` — at least one deterministic-tier annotation
  (`[check]`, `[test]`, or `[system]`) on the bead must exercise the
  live path; the bead's full set bypasses it.
- `fabricated-result` — the verifier's pass relies on a value the
  test itself synthesized.
- `weak-assertion` — the assertion tautologically passes.
- `coincidental-pass` — the test passes for the wrong reason.

The remaining tokens cover the other rubric dimensions:

- `mock-discipline` — a mock stands in for the very thing the test
  claims to test.
- `verifier-too-narrow` — a multi-component contract has a verifier
  that exercises only one side of the seam.
- `concurrency-untested` — production code introduces or modifies
  shared-state synchronisation primitives without at least one
  concurrent-load test.
- `judge-flag` — a `[judge]` rubric is not satisfied.
- `style-rule-violation` — the diff violates a rule in the pinned
  style-rules document; the `target.rule_id` names the violating rule,
  `target.subject` names the stable violated surface, and the
  `evidence` cites file/line range.
- `spec-coherence-fail` — a claim in a touched spec section is not
  realised by the code (no passing verifier and no LLM trace).
- `orphan-integration` — a multi-component contract spans beads but
  the closure is not complete in the molecule's diff or bonded
  siblings.
- `invariant-clash` — a load-bearing invariant in the touched spec
  set is silently contradicted by the diff. **Embed the canonical
  `## Options — …` block in `evidence`** using the exact heading
  shape below (prose `Recommended:` / `Alternative:` headings do
  NOT count and degrade the minted bead to `loom:blocked` with
  cause `clarify-without-options`):

{% include "partial/options_format.md" %}

  The driver attaches `loom:clarify` to the minted bead and lifts
  the block into its description.
- `template-spec-drift` — at `--tree` scope, a prompt template under
  `crates/loom-templates/templates/` directs agents toward behaviour
  a spec contradicts (Invariant 3 from `specs/gate.md`).

### Streaming + terminator pairing rule

The walk is a streaming process: `LOOM_FINDING:` lines are emitted
as concerns are identified; a single terminator is the final line.
The driver cross-checks the two — if the terminator and the stream
disagree on the walk's verdict, the run fails with a typed
`BadWalk` recovery cause.

`LOOM_CONCERN:` is the verdict-carrying terminator for a walk that
streamed at least one `LOOM_FINDING:` line; `LOOM_COMPLETE` is the
clean-walk terminator. The payload of `LOOM_CONCERN:` is a JSON
object with a single required field, `summary`, whose value is a
non-empty string:

```text
LOOM_CONCERN: {"summary":"<one-sentence summary of the strongest concern>"}
```

The driver parses the payload with the same `serde_json` pipeline
that consumes `LOOM_FINDING:` lines. Parse failures — invalid JSON,
missing `summary`, empty `summary` — surface as the typed
`BadWalk::Concern { payload }` recovery cause. The summary is for the
verdict log only; per-finding routing is decided from each streamed
finding's `route`, not from the terminal marker.

| Findings streamed | Terminator | Verdict |
|---|---|---|
| 0 | `LOOM_COMPLETE` | clean — phase done |
| ≥1 | `LOOM_CONCERN: {"summary":"..."}` | recovery — findings minted, summary threaded into `previous_failure` |
| 0 | `LOOM_CONCERN: {...}` | `BadWalk::ConcernWithoutFindings { summary }` — concern claimed without enumeration |
| ≥1 | `LOOM_COMPLETE` | `BadWalk::FindingsWithoutConcern { finding_count }` — findings streamed but terminator claims clean |
| any | `LOOM_CONCERN:` with malformed JSON / missing / empty `summary` | `BadWalk::Concern { payload }` — payload parse failure subsumes any finding count |
| any | missing or duplicate marker | `SwallowedMarker` (existing) |

**Agent's mental model.** Review the diff. Every time you identify a
concern, immediately emit a `LOOM_FINDING:` line with the structured
JSON detail and continue reviewing. When the walk is complete, end
your response with `LOOM_COMPLETE` if you found nothing, or
`LOOM_CONCERN: {"summary":"<one-sentence summary>"}` if you emitted
one or more `LOOM_FINDING:` lines. The terminator must match the
stream: `LOOM_COMPLETE` means zero findings, `LOOM_CONCERN` means
≥1 finding. The two are mutually exclusive — never emit both, and
never alongside any other marker.

A walk that emits `LOOM_FINDING:` lines without a terminal marker is
a crashed run; the driver fails the mint invocation with non-zero
exit. Emitting `LOOM_CONCERN:` from any non-review phase is a
`wrong-phase-marker` error in the verdict gate.

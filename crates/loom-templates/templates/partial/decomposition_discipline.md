## Decomposition Discipline

Every bead you author in this session must correspond to **evidence-confirmed
missing work**, not to a Success Criteria bullet considered in the abstract. A
spec criterion that already has a fresh, honest verifier verdict is positive
evidence of coverage, not a candidate for a new implementation bead.

### Audit before fan-out

Before authoring any non-audit bead, you MUST:

1. **Consult the `criterion_status` surface** rendered above for each
   criterion in scope. Each row exposes the criterion's annotation target,
   the cached verifier verdict (`Pass | Fail | Skipped | NoResult`), the
   timestamp of the run that produced that verdict, the commit it was
   recorded against, and how many commits have landed on HEAD since. A
   row whose verdict is `Pass`, whose timestamp is recent, and whose
   commits-since count is low is positive evidence of coverage — do
   **not** author a bead for it.

2. **Inspect representative implementations and verifier functions** for
   any row that is suspicious: a stale timestamp, a large commits-since
   value, a `NoResult` row on a fresh checkout, or a verifier-target
   name whose body may not actually exercise the live system. A
   directory listing proves the file exists; it does not prove the
   file contains the named target, and it does not prove the named
   target exercises the contract the criterion asserts. Read the
   verifier body, and read the production code path the verifier
   claims to cover.

The discipline is template-agnostic: it applies to whatever workspace
layout the consumer has. "Representative implementations" means the
production code path the criterion's verifier is supposed to exercise,
under whatever organisation that consumer's repository uses.

### Two acceptable session outcomes

A decomposition session has **exactly two** acceptable outcomes:

- **(a) Gap-targeted bead set.** You author beads only for criteria the
  audit confirms are missing, incomplete, or covered by a dishonest
  verifier (one that asserts a tautology, mocks the thing it claims to
  test, or otherwise passes for the wrong reason). For each such bead,
  cite the evidence that drove its creation in the bead description:
  the `criterion_status` row you consulted (annotation target, last
  verdict, timestamp/commits-since), the file you read that surfaced
  the gap, and/or the verifier-source observation that showed the
  target was dishonest. A bead without an evidence citation is a
  decomposition failure — the gate's review walk treats it as a
  fan-out by axis rather than by gap.

- **(b) Clarify on the molecule epic.** When coverage cannot be
  determined by inspection — the spec is ambiguous, verifier targets
  conflict, or your judgement of the status cache's trustworthiness
  is itself contestable — emit `LOOM_CLARIFY`. Persist both the
  question and the canonical `## Options — …` block to the **molecule
  epic's** notes per the *Options Format Contract* in `specs/gate.md`
  before emitting the marker. The verdict gate applies
  `loom:clarify` to the epic; the human resolves via `loom msg
  <epic>`, and a subsequent `loom todo` invocation consumes the
  answer from the epic's notes before fanning out.

Per-bead `loom:clarify` is not appropriate here: in `todo_new` the
child beads do not yet exist, and in `todo_update` they are exactly
the set under negotiation. The molecule epic is the only
session-stable carrier for "this molecule's decomposition is paused
pending clarification."

### Epic-first-always ordering

For the clarify-on-epic fallback to be viable mid-decomposition, the
molecule epic must exist **before** any criterion-by-criterion gap
analysis runs. In `todo_new` flows, create the molecule epic as the
first authoring step — before reading `criterion_status` rows for
gap analysis, before authoring any child bead. In `todo_update`
flows the molecule already exists, so the ordering is automatic.

Without this ordering, hitting an audit ambiguity mid-decomposition
would leave the clarify with no valid target — the very failure mode
the discipline is meant to prevent.

### Enumerate-everything fan-outs are structurally invalid

A fixed decomposition axis applied across the board irrespective of
evidence — for example, "setup, implementation, tests, documentation"
mechanically expanded onto every criterion — is precisely the failure
mode this discipline targets. The combined effect of (i) the
`criterion_status` surface exposing positive evidence that whole axes
already pass and (ii) the audit-before clause's evidence-confirmation
prerequisite for bead authorship makes such fan-outs structurally
unviable. `loom gate review`'s judge-tier walk is what catches any
decomposition that bypasses the `criterion_status` surface to
re-introduce enumerate-everything beads.

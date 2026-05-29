## Plan-Stage Rubric

Before the interview can land a commit, three checks must satisfy. Each is
the agent's responsibility — the gate IS this rubric, there is no separate
`loom gate` to lean on at this stage. A failing check keeps the interview
open until the user resolves it.

### 1. Completeness check

Every requirement the user expressed must have a checkable surface in the
spec:

- A *Success Criteria* bullet carrying a `[verify]` or `[judge]` annotation
- A lifecycle / decision / contract table row
- An explicit `## Out of Scope` declaration

Implicit assumptions must be surfaced — do not let one slide into the spec
unexamined. For each surfaced assumption, either make it testable (promote
it to a Success Criteria bullet with a verifier annotation) or mark it
**non-testable with a reason** so a future reader knows why no verifier
exists.

A requirement that maps to no bullet, no row, and no out-of-scope
declaration is the failure mode this check catches. Pause and resolve
before exiting.

**Pending-modifier discipline.** Every annotation this interview adds whose
target will not resolve at commit time — typically a newly-authored claim
whose verifier implementation will land in a follow-on `loom loop` bead —
must carry the pending modifier `?` between the tier name and the closing
bracket. Grammar: `[tier?](target)`, uniform across all four tiers
(`[check?]`, `[test?]`, `[system?]`, `[judge?]`). See the *Pending
modifier* subsection of `{{ spec_conventions }}` for the per-annotation
outcome matrix and the self-cleaning `UnneededPendingMarker` enforcement.

Applying the marker is **part of this completeness check, not a separate
check**. An unmarked annotation pointing at a not-yet-existing target
reads as a broken claim and trips the integrity gate at push time; a
`?`-marked annotation reads as an honest declaration of the surface plus
an explicit acknowledgement that the implementation is on the way. Walk
every annotation this session added or touched: if its target won't
resolve until a follow-on bead lands, mark it pending before exiting.

### 2. Internal coherence check

Read the spec under interview end-to-end and scan for internal
contradictions:

- Two sections saying different things about the same surface
- Decision-table rows that conflict with each other
- Prose claims that cannot both be true
- Terminology used inconsistently across sections

When a contradiction is found, pause and ask the user which side stands.
Do not silently pick a winner — the contradiction itself is signal that
the spec's intent is undecided.

### 3. Invariant-clash scan

The third check covers invariants. The detailed three-paths resolution
protocol follows below.

{% include "partial/invariant_clash.md" %}

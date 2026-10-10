## Plan-Stage Rubric

Before the interview can land a commit, three checks must satisfy. Each is
the agent's responsibility — the gate IS this rubric, there is no separate
`loom gate` to lean on at this stage. A failing check keeps the interview
open until the user resolves it.

### 1. Completeness check

Every requirement the user expressed must have a checkable surface in the
owning package:

- A _Success Criteria_ bullet in `tests.md` carrying exactly one `[check]`, `[test]`, `[system]`, or `[judge]` annotation
- An explicit `## Out of Scope` declaration in `spec.md`

**Contract coverage.** Map every behavioral contract section and every lifecycle,
decision, or contract table row to named sections in the same package's
`tests.md` using ordinary Markdown links, for example
`[Admission](tests.md#admission)`. A section-level link may cover multiple rows
when the linked criteria cover every row; repeating the link in each row is
unnecessary. Check that destinations and heading fragments resolve, and update
incoming links when headings change. Keep verifier bindings beside the criteria;
do not duplicate annotations in `spec.md` or invent a mapping registry.

Assess actual semantic coverage: the linked criteria must capture all behavior
asserted by the section or row, including normative prose and implicit claims.
A valid link, an unrelated verifier, or equal row/annotation counts is not proof
of coverage. Unmapped prose or rows and linked-but-irrelevant criteria fail this
check. In the flat bootstrap tree before cutover, use the same owner's inline
Success Criteria and ordinary local section links instead of missing package
paths; the same coverage and exactly-one-binding rules apply.

Surface implicit functional assumptions and convert them into annotated,
checkable claims. Move genuine non-goals to `Out of Scope` with their
rationale, rather than leaving unverifiable contract text in the spec.

A requirement that maps to neither an annotated criterion nor an explicit
out-of-scope declaration fails this check. Pause and resolve before exiting.

**Pending-modifier discipline.** Every annotation this interview adds whose
target will not resolve at commit time — typically a newly-authored claim
whose verifier implementation will land in a follow-on `loom loop` bead —
must carry the pending modifier `?` between the tier name and the closing
bracket. Grammar: `[tier?](target)`, uniform across all four tiers
(`[check?]`, `[test?]`, `[system?]`, `[judge?]`). See the _Pending
modifier_ subsection of `{{ spec_conventions }}` for the per-annotation
outcome matrix and the self-cleaning `UnneededPendingMarker` enforcement.

Applying the marker is **part of this completeness check, not a separate
check**. An unmarked annotation pointing at a not-yet-existing target
reads as a broken claim and trips the integrity gate at push time; a
`?`-marked annotation reads as an honest declaration of the surface plus
an explicit acknowledgement that the implementation is on the way. Walk
every annotation this session added or touched: if its target won't
resolve until a follow-on bead lands, mark it pending before exiting.

**Binary-pending vs assertion-pending.** Binary-pending means an honestly
prospective executable, referenced path, or execution definition is absent,
for example `[check?](cargo run -p future-walker -- admission)` before that
walker exists. Assertion-pending means the full admitted command completes
with a nonzero assertion because the predicate is not yet true, for example
`[check?](grep -q 'pub enum NewVariant' crates/foo/src/existing.rs)` before
the symbol is implemented. Both shapes warrant `?`; neither is a requirement
pass.

**Readiness admission boundary.** Executable `[check?]` and `[system?]` probes
use Verify's shared admitted planner: provider admission, affectedness,
requested tier/lane and stage, sharing, execution budget, and dispatch environment
are the same as ordinary verification, with readiness as a distinct purpose.
Honest prospective absence remains pending without executing an unregistered
command to prove absence. Once a provider offers an executable definition,
malformed metadata, inconsistent inputs, or failed discovery are admission
errors; `?` cannot suppress them. Unavailable required host capabilities are
not honest absence or assertion-pending. Capability skips, timeouts, interrupted
or incomplete execution cannot count as completed required readiness or passing
evidence. Do not excuse arbitrary spawn errors with `?`.

Evaluate the full admitted command, not just executable existence: completed
nonzero assertions stay pending; exit 0 fires `UnneededPendingMarker` and
requires removal of `?`. Readiness results never supply ordinary pass evidence
for the pending criterion, and sharing execution cannot hide an ordinary
sibling's failure or discharge its coverage. Follow Verify's _Pending modifier_
rule, located through the spec index, rather than running a separate probe
launcher from the interview.

**Added and modified annotations both count.** If this session adds an
annotation or changes an existing annotation's command, file path,
grep pattern, or symbol name so the new target does not resolve now,
mark it pending. Do not treat modified annotations as exempt.

**Structured walker input uses pending cells.** Planning edits to
matrix rows, surface tables, or other structured walker input use the
walker's own `?` pending-addition and `~` pending-removal cell syntax in
the input element itself, not the success-criterion annotation marker.
Follow Verify's _Pending support in structured walker input_ rule: silent-pass
while the state matches the pending direction, then fire
`pending-marker-resolved` when the state catches up.

**Self-cleaning obligation.** Drop the pending marker in the same diff
that resolves the target: `[tier?]` becomes `[tier]`, `?` cells become
`✓`, and `~` cells become blank. Leaving it behind fires
`UnneededPendingMarker` or the structured walker's pending-marker-
resolved finding.

**Atomic-acceptance discipline.** Each Success Criteria bullet carries
**exactly one** verifier annotation. A bullet that needs two is two
criteria — split. The integrity gate flags multi-annotation criteria
with the `MultipleAnnotations` finding, so silent fan-out becomes a
loud push-time failure.

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

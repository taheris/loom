# Package-coverage probes

The adjacent `coverage/` directories are candidate owner documents, not live
specs. `planning_guidance` runs real package discovery, link checking,
acceptance parsing, and planning-prompt rendering against each candidate. Its
observed structural results below deliberately do not decide semantic coverage.
The interview continuations are representative scripted counterexamples, not
recorded model executions or passing verification evidence.

## Unmapped prose and rows

Input: `coverage/unmapped/{spec,tests}.md`. Observed native result: one
criterion, one pending binding, valid Admission link. The parser does not claim
semantic completeness.

Proposed continuation: "Admission has a test, so this package is complete."
Reject it: duplicate submission's original-ID/no-second-job behavior is absent
from acceptance, and both cancellation rows lack a mapping. A verifier for ID
assignment does not cover the other prose in the same section.

Correct continuation: identify those three missing behaviors and keep the
interview open to add acceptance or resolve intent with the user. Do not create
implementation code or beads to compensate for missing planning coverage.

## Equal counts, irrelevant criteria

Input: `coverage/equal-count/{spec,tests}.md`. Observed native result: two
criteria, two pending bindings, valid Lifecycle link.

Proposed continuation: "Two rows, two tests, valid link: completeness passes."
Reject it: sorting names and ASCII IDs cover neither queued cancellation nor
running cancellation/lease release. Structural validity is not semantic
coverage. Duplicating those annotations in the table would not repair it.

## Valid split package with section-level links

Input: `coverage/split/{spec,tests}.md`. Observed native result: three criteria,
three pending bindings; both links resolve. The Cancellation section has one
link over two rows and one criterion covering both state-specific outcomes.
Bindings occur only in `tests.md`.

Proposed continuation: "The mapped criteria capture ID assignment, duplicate
admission, and both cancellation outcomes; no row-level repeated link is needed.
The targets are prospective, not passing evidence. Shall we continue?" Accept
this coverage assessment. Reject a demand to put an annotation in every row,
copy criteria into `spec.md`, or add a registry. This accepts the checkable
contract, not an assertion that the pending tests have run.

## Readiness outcome contrasts

Evaluate these against the delivered planning guidance:

- A prospective walker/definition honestly does not exist: binary-pending is
  allowed without executing an unregistered command to prove absence.
- A full admitted grep command completes nonzero while its proposed symbol is
  absent: assertion-pending is allowed, not a requirement pass.
- The same admitted assertion exits zero: remove `?` in the resolving diff;
  `UnneededPendingMarker` is not optional cleanup.
- An offered provider has invalid metadata or inconsistent dependencies, or its
  discovery query fails: reject "add `?` to bypass the failure."
- A required host capability is unavailable, or execution skips, times out, or
  is interrupted: reject "nonzero means still pending, so readiness completed."
- A shared unit serves a pending criterion and an ordinary failing sibling:
  reject using the pending declaration to hide the sibling's failure.

These outcome contrasts are semantic probes, not a second readiness runner.
Verify owns admission, scope/stage selection, budget and dispatch; the interview
must not launch its own alternate provider path.

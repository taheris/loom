## Self-Report Markers

When you cannot complete the work, end your response with one of
these markers instead of a progress marker. They are mutually
exclusive with each other **and** with the progress markers — emit
exactly one, on its own line, as the final output of the session.

- `LOOM_RETRY` — This attempt cannot finish but a fresh dispatch is
  likely to succeed. Two failure shapes warrant retry: (a)
  **environmental** — tools failing mid-session, sandbox/cwd
  unlinked, transient IO — and (b) **agent self-reset** —
  stuck-but-not-blocked, prompt-context exhausted, approach
  abandoned. Write the reason on the line immediately before the
  marker; the driver captures it verbatim as
  `PreviousFailure::AgentRetry { reason }` for the next attempt's
  prompt. Each `LOOM_RETRY` consumes one slot in `[loop] max_retries`
  (default 2); exhaustion escalates to `loom:blocked` with cause
  `retry-exhausted`. If the same problem persists after retry,
  escalate to `LOOM_BLOCKED` only for a semantic dead end whose
  prior-line reason explains why candidate options cannot be safely
  enumerated, or to `LOOM_CLARIFY` when a structured Options block can
  frame the candidate resolutions, rather than emitting `LOOM_RETRY`
  again.
- `LOOM_CLARIFY` — A decision is **blocking your work** and has
  two or more genuinely viable resolutions you cannot adjudicate
  from the spec, the code, or research. **Not for ratifying a
  recommended path** — if you can write "Recommended: X.
  Alternative: Y" and X is your clear preference, you do not have
  a clarify question, you have a plan; file or implement it
  directly. Reserve `LOOM_CLARIFY` for the cases where the spec
  is genuinely ambiguous, two paths carry materially different
  costs or risks you cannot weigh, or your judgement of the
  evidence is itself contestable.

  **Persist the question and the canonical options block to the target
  bead/work epic before emitting the marker** — use `bd update --notes` or
  `bd update --description` on the bead under dispatch (`loop`) or on the
  injected `loom:todo` work epic (`todo`), per the Options Format Contract
  in `specs/gate.md`. Do not create a separate clarify bead for direct
  `LOOM_CLARIFY`; the verdict gate validates the target bead/work epic's
  persisted block and applies `loom:clarify`. The gate
  does NOT parse your prose for options: prose `Recommended:` /
  `Alternative:` headings are NOT the canonical block and will downgrade
  the bead to `loom:blocked` with cause `clarify-without-options`.
  The block MUST use the canonical heading shape:

{% include "partial/options_format.md" %}

  After persisting, the gate applies `loom:clarify` to the target
  bead/work epic and exits without entering recovery; other beads in the
  molecule continue running. The labelled bead or work epic waits for
  `loom inbox` resolution.
- `LOOM_BLOCKED` — Genuine semantic dead end: you cannot proceed,
  do not expect retry to succeed, and cannot safely enumerate candidate
  resolutions. Write a non-empty reason on the line immediately before
  the marker explaining why options cannot be safely enumerated (the gate
  only reads the most recent non-empty prior line — multi-paragraph prose
  is NOT captured). The gate applies `loom:blocked` to the target
  bead/work epic and exits without entering recovery; other beads in the
  molecule continue running. The labelled bead or work epic waits for
  human resolution via `loom inbox`. **If you can enumerate options, do
  NOT use `LOOM_BLOCKED`** — use `LOOM_CLARIFY` above so the candidate
  resolutions reach bead state.

**Discriminator.** expect retry to succeed? → RETRY. Can you
safely enumerate candidate options for the human? → CLARIFY (a
recommended path with prose alternatives is NOT clarify — file the plan
or implement it directly). Semantic dead end with no safe options to
surface? → BLOCKED with the no-options rationale on the prior line.

**Direct worker self-reports only.** These instructions apply to direct
self-reports from `loop` and `todo`. `review` uses a review-specific
self-report partial because review is inspection-only and routes
clarify-worthy decisions through finding evidence instead of direct bd
persistence. Interactive sessions (`plan`, `inbox`) do not emit worker
self-report markers — `plan` ends with `LOOM_COMPLETE`, while `inbox`
ends with `LOOM_COMPLETE` or `LOOM_APPLY: {"proposals":[...]}` when the
trusted driver apply handoff is requested. The human is in the room and
resolves friction in-turn, so the cannot-finish terminators are out of
scope for those templates.

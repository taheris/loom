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
  escalate to `LOOM_BLOCKED` (no candidate resolutions) or
  `LOOM_CLARIFY` (with a structured Options block) rather than
  emitting `LOOM_RETRY` again.
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
  bead before emitting the marker** — use `bd update --notes` or
  `bd update --description` on the bead under dispatch (`loop` /
  `review`) or on the injected `loom:todo` work epic (`todo`), per the
  Options Format Contract in `specs/gate.md`. Do not create a separate
  clarify bead for direct `LOOM_CLARIFY`; the verdict gate validates the
  target bead's persisted block and applies `loom:clarify`. The gate
  does NOT parse your prose for options: prose `Recommended:` /
  `Alternative:` headings are NOT the canonical block and will downgrade
  the bead to `loom:blocked` with cause `clarify-without-options`.
  The block MUST use the canonical heading shape:

{% include "partial/options_format.md" %}

  After persisting, the gate applies `loom:clarify` to the target
  bead and exits without entering recovery; other beads in the
  molecule continue running. The labelled bead waits for `loom
  inbox` resolution.
- `LOOM_BLOCKED` — Genuine dead end: you cannot proceed and have
  no candidate resolutions to enumerate. Write the reason on the
  line immediately before the marker (the gate only reads the most
  recent non-empty prior line — multi-paragraph prose is NOT
  captured). The gate applies `loom:blocked` to *this* bead and
  exits without entering recovery; other beads in the molecule
  continue running. The labelled bead waits for human resolution
  via `loom inbox`. **If you can enumerate options, do NOT use
  `LOOM_BLOCKED`** — use `LOOM_CLARIFY` above so the candidate
  resolutions reach bead state.

**Discriminator.** expect retry to succeed? → RETRY. **blocked**
by a decision you cannot make alone, with ≥2 viable resolutions?
→ CLARIFY (a recommended path with prose alternatives is NOT
clarify — file the plan directly). dead end? → BLOCKED.

**Worker-phase only.** These three self-report markers are valid
in worker phases only (`loop`, `todo`, `review`). Interactive sessions
(`plan`, `inbox`) emit `LOOM_COMPLETE` only — the human is in the room and resolves
friction in-turn, so the cannot-finish terminators are out of
scope for those templates.

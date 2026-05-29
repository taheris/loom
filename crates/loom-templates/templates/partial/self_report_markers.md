## Self-Report Markers

When you cannot complete the work, end your response with one of
these markers instead of a progress marker. They are mutually
exclusive with each other **and** with the progress markers — emit
exactly one, on its own line, as the final output of the session.

- `LOOM_BLOCKED` — You cannot proceed and are self-reporting
  *without* a menu of resolution options. Write the reason on the
  line immediately before the marker (the gate only reads the most
  recent non-empty prior line — multi-paragraph prose is NOT
  captured). The gate applies `loom:blocked` to *this* bead and exits
  without entering recovery; other beads in the molecule continue
  running. The labelled bead waits for human resolution via
  `loom msg`. **If you have multiple candidate resolutions, do NOT
  use `LOOM_BLOCKED`** — use `LOOM_CLARIFY` below so the options
  reach bead state.
- `LOOM_CLARIFY` — You have a specific question with structured
  options for the human. **Persist the question and the canonical
  `## Options — …` block to bead state before emitting the marker**
  — either by `bd create` on a new clarify bead or by `bd update
  --notes` + `bd update --add-label=loom:clarify` on an existing bead
  — per the Options Format Contract in `specs/gate.md`. The gate does
  NOT parse your prose for options; if the canonical block lives only
  in your stdout, `loom msg`'s queue will be empty. After persisting,
  the gate applies `loom:clarify` to *this* bead and exits without
  entering recovery; other beads in the molecule continue running.
  The labelled bead waits for `loom msg` resolution.

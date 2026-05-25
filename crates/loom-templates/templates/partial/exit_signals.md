## Exit Signals

End your response with exactly **one** of these markers on its own line, as
the final output of the session. The orchestrator parses **only the final
non-empty line** verbatim to derive the gate's verdict — markers emitted on
any earlier line are treated as `swallowed-marker`, and multiple markers on
the final line are likewise rejected. Markers are mutually exclusive: emit
one and only one.

- `LOOM_COMPLETE` — The work succeeded. For worker phases (`loom run`), this
  also means the bead's acceptance criteria are met and the bead has been
  closed via `bd close`. The diff must be non-empty (real changes); see
  `LOOM_NOOP` below for the zero-diff variant.
- `LOOM_NOOP` — The work was already done in tree and this phase
  intentionally produced an empty diff. Close the bead with `bd close`
  before emitting. Use `LOOM_NOOP` instead of `LOOM_COMPLETE` whenever the
  diff is empty — an empty diff with `LOOM_COMPLETE` is treated as
  `zero-progress` and enters recovery. Only valid in worker phases.
- `LOOM_BLOCKED` — You cannot proceed and are self-reporting *without* a
  menu of resolution options. Write the reason on the line immediately
  before the marker (the gate only reads the most recent non-empty prior
  line — multi-paragraph prose is NOT captured). The gate applies
  `loom:blocked` to *this* bead and exits without entering recovery; other
  beads in the molecule continue running. The labelled bead waits for
  human resolution via `loom msg`. **If you have multiple candidate
  resolutions, do NOT use `LOOM_BLOCKED`** — use `LOOM_CLARIFY` below so
  the options reach bead state.
- `LOOM_CLARIFY` — You have a specific question with structured options for
  the human. **Persist the question and the canonical `## Options — …`
  block to bead state before emitting the marker** — either by `bd create`
  on a new clarify bead or by `bd update --notes` + `bd update
  --add-label=loom:clarify` on an existing bead — per the Options Format
  Contract in `specs/gate.md`. The gate does NOT parse your prose for
  options; if the canonical block lives only in your stdout, `loom msg`'s
  queue will be empty. After persisting, the gate applies `loom:clarify`
  to *this* bead and exits without entering recovery; other beads in the
  molecule continue running. The labelled bead waits for `loom msg`
  resolution.
- `LOOM_CONCERN: <token> -- <reason>` — **Review phase only.** The review
  found a quality issue with the molecule's work; push must not fire.
  `<token>` is one of the concern tokens (see the Flag Emission Schema in
  the review template) and `<reason>` is a one-sentence summary. The
  review phase emits `LOOM_CONCERN` xor `LOOM_COMPLETE` — never both, and
  never alongside any other marker. Emitting `LOOM_CONCERN` from any
  non-review phase is a `wrong-phase-marker` error in the verdict gate.

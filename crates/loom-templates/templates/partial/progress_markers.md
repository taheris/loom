## Progress Markers

End your response with exactly **one** marker on its own line, as
the final output of the session. The orchestrator parses **only the
final non-empty line** verbatim to derive the gate's verdict —
markers emitted on any earlier line are treated as
`swallowed-marker`, and multiple markers on the final line are
likewise rejected. Markers are mutually exclusive: emit one and
only one.

Progress markers report that the session's work is **done**. Their
phase-specific meaning is:

- `LOOM_COMPLETE` — The phase succeeded. In review, this means a
  clean inspection with zero findings; no diff is expected and
  `bd close` is not part of review. In interactive plan/inbox
  sessions, it ends the human-in-the-loop chat. In `loom loop`, it
  also means the bead's acceptance criteria are met, the bead has
  been closed via `bd close`, and the diff is non-empty (real
  changes); see `LOOM_NOOP` below for the loop zero-diff variant.
- `LOOM_NOOP` — Loop-only success: the work was already done in tree
  and this `loom loop` phase intentionally produced an empty diff.
  Close the bead with `bd close` before emitting. Use `LOOM_NOOP`
  instead of `LOOM_COMPLETE` whenever the loop diff is empty — an
  empty loop diff with `LOOM_COMPLETE` is treated as `zero-progress`
  and enters recovery. `LOOM_NOOP` is not a review, plan, todo, or
  inbox terminal.

If this phase pins self-report markers and you cannot finish — needing
human input or a decision among multiple resolutions — emit one of the
self-report markers documented separately (see *Self-Report Markers*).
Interactive plan/inbox sessions resolve friction with the human in-turn
instead of worker self-report markers.

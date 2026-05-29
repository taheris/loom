## Progress Markers

End your response with exactly **one** marker on its own line, as
the final output of the session. The orchestrator parses **only the
final non-empty line** verbatim to derive the gate's verdict —
markers emitted on any earlier line are treated as
`swallowed-marker`, and multiple markers on the final line are
likewise rejected. Markers are mutually exclusive: emit one and
only one.

Progress markers report that the session's work is **done**:

- `LOOM_COMPLETE` — The work succeeded. For worker phases
  (`loom loop`), this also means the bead's acceptance criteria are
  met and the bead has been closed via `bd close`. The diff must be
  non-empty (real changes); see `LOOM_NOOP` below for the zero-diff
  variant.
- `LOOM_NOOP` — The work was already done in tree and this phase
  intentionally produced an empty diff. Close the bead with
  `bd close` before emitting. Use `LOOM_NOOP` instead of
  `LOOM_COMPLETE` whenever the diff is empty — an empty diff with
  `LOOM_COMPLETE` is treated as `zero-progress` and enters recovery.
  Only valid in worker phases.

If you cannot finish — needing human input or a decision among
multiple resolutions — emit one of the self-report markers
documented separately (see *Self-Report Markers*).

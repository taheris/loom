## Review Self-Report Markers

Review is inspection-only. Do not mutate `bd` state or persist review
self-report state with bd commands. A completed review reports concerns
through the streaming finding protocol above; these self-report markers are
only for cases where the review walk itself cannot complete. When you use
one, place exactly one marker on the final non-empty line.

- `LOOM_RETRY` — The review attempt cannot finish but a fresh dispatch is
  likely to succeed. Use this for environmental failures such as corrupt
  logs, inaccessible workspace state, transient IO, or a missing prerequisite
  that should be present. Write the reason on the line immediately before
  the marker; the driver records it as retry context.
- Clarify-worthy review decisions do **not** use direct `LOOM_CLARIFY`.
  If you can enumerate candidate resolutions, emit a `route="clarify"`
  `LOOM_FINDING` whose `evidence` embeds the canonical `## Options — …`
  block, then finish the walk with `LOOM_CONCERN`. If you cannot articulate
  options and the review cannot proceed, use `LOOM_BLOCKED` instead.
- `LOOM_BLOCKED` — The review cannot proceed and you have no candidate
  resolutions to enumerate. Write the dead-end reason on the line
  immediately before the marker. Do not create, update, label, or close
  beads from the review prompt.

**Discriminator.** expect retry to succeed? → RETRY. can you enumerate
options? → emit a clarify-route finding. dead end with no options? →
BLOCKED.

**Review-phase only.** These self-report rules apply only to `review`.
`loop` and `todo` use the direct worker self-report partial; interactive
sessions (`plan`, `inbox`) do not emit worker self-report markers.

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
  If you can safely enumerate candidate resolutions, emit a
  `route="clarify"` `LOOM_FINDING` whose `evidence` embeds the canonical
  `## Options — …` block, then finish the walk with `LOOM_CONCERN`. If
  the review cannot proceed and no options can be safely articulated, use
  `LOOM_BLOCKED` instead.
- `LOOM_BLOCKED` — The review cannot proceed and you have no candidate
  resolutions to enumerate. Write a non-empty dead-end reason on the line
  immediately before the marker explaining why options cannot be safely
  surfaced. Do not create, update, label, or close beads from the review
  prompt.

**Discriminator.** expect retry to succeed? → RETRY. Can you safely
surface candidate options? → emit a clarify-route finding. Semantic dead
end with no safe options? → BLOCKED with the no-options rationale on the
prior line.

**Review-phase only.** These self-report rules apply only to `review`.
`loop` and `todo` use the direct worker self-report partial; interactive
sessions (`plan`, `inbox`) do not emit worker self-report markers.

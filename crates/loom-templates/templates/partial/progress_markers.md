## Progress Markers

End the session with exactly **one final logical terminal**, mutually exclusive
with every other terminal. The compiled Canonical Agent-Output Syntax table
above supplies this phase's spelling, role, and unit/data arity. Nonterminal
records may precede it; no commentary may follow it. A pretty-printed JSON
object is one logical message, not several physical-line markers.

{% if self.output_phase() == loom_protocol::output::Phase::Loop %}
- `LOOM_COMPLETE` — The bead's acceptance criteria are met, the bead is closed
  via `bd close`, and the diff is non-empty (real changes).
- `LOOM_NOOP` — Loop-only success: work was already done in tree and this
  session intentionally produced an empty diff. Close the bead via `bd close`.
  An empty loop diff with `LOOM_COMPLETE` enters zero-progress recovery.

Success examples (choose only the one matching the actual diff):

```text loom-example
LOOM_COMPLETE
```

```text loom-example
LOOM_NOOP
```
{% else if self.output_phase() == loom_protocol::output::Phase::Review %}
A clean inspection ends with `LOOM_COMPLETE`: zero findings, no expected diff,
and no `bd close`. Findings instead require the Concern terminal documented
above; never emit both. `LOOM_NOOP` is not a review, plan, todo, or inbox terminal.

```text loom-example
LOOM_COMPLETE
```
{% else %}
`LOOM_COMPLETE` ends the human-in-the-loop planning chat, not an implementation
session. Planning does not authorize code, model, or Beads writes. Emit this
only on the final assistant turn after explicit session-close consent.

```text loom-example
LOOM_COMPLETE
```
{% endif %}

Worker cannot-finish outcomes and decision records are documented separately.
Interactive sessions resolve friction with the human in-turn instead of worker
self-reports. Do not confuse decision discovery with success or source waiting.

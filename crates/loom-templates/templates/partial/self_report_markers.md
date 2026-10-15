## Decision Records and Self-Reports

Decision discovery is independent of the session's terminal outcome. Loop and
Todo report all currently discovered unresolved human decisions through
nonterminal records, never a bare `LOOM_CLARIFY` terminal:

The `decisions` array must be nonempty and contain dedicated decision bead IDs,
not source/task IDs. Repeated IDs identify one decision. The terminal remains
independent. For example, after staging both decisions and declaring the source's
actual prerequisite, report them and wait:

```text loom-example
LOOM_CLARIFY: {
  "decisions": ["lm-decision.1", "lm-decision.2"]
}
LOOM_WAITING
```

Each decision has one dedicated bead with one canonical Options brief. Parent it
directly under the discovering task (Loop) or injected work epic (Todo); do not
put unrelated briefs on the source or label the source `loom:clarify`. Create
new candidates with `status=blocked`, without `loom:clarify`, `loom:blocked`, or
`loom:infra` queue labels. Persist producer/source and work-root provenance
alongside the brief and proposed edges; the driver admits references and briefs
before queue or scheduling effects. Reuse compatible existing decisions rather
than duplicating them. Report the full known set, including persisted candidates
from an interrupted attempt; do not leave an invisible unreported decision.
Account for **all owned persisted candidates** before any terminal, including
ones affecting only other work. Valid prior dispositions and historical closed
decisions remain accounted for without redundant emissions. Candidate metadata
is an untrusted proposal, not task acceptance or queue/wait authority. A failed
reference/graph handoff retains candidates for repair, never silently treats
them as implementation tasks. The driver admits valid briefs to `loom:clarify`
and downgrades defective briefs on that decision to `loom:blocked`; the source's
outcome remains independent. Workers own the brief, ordinary bead creation,
labels, descriptions and proposed affected-work edges; do not copy Options
briefs onto the source or work epic.

{% if self.output_phase() == loom_protocol::output::Phase::Loop %}
For each new blocked Loop candidate, persist `loom.decision_candidate` metadata
as a JSON string with exactly these fields:
`{"version":1,"producer":"<source-id>","root":"<work-root-id>","affected_work":["<affected-work-id>"]}`.
Use actual IDs from this dispatch, not these placeholders. `affected_work` is the
complete distinct set of work beads that depend on this decision through
supported native blocking edges; it may be empty. Do not include the discovery-child producer merely
because it requests its own driver hold: that is **not** a native blocking edge.
The driver independently reads native edges, checks work-root membership and
binds this proposal to the admitted inventory. A changed or uncertain graph
cannot reuse an old disposition. Legacy discovery children retain their native
IDs and parentage; worker metadata never replaces driver attribution.

{% endif -%}
{% include "partial/options_format.md" %}

Only work whose acceptance genuinely requires a decision gets a prerequisite.
For supported non-descendant prerequisites, use
`bd dep add <affected-work> <decision>`. Never add a forbidden descendant edge,
rename decision IDs, or bypass native ancestry/cycle guards. Parentage alone is
not a prerequisite.
If the source itself must wait, declare its active prerequisites, leave it open,
then end with `LOOM_WAITING`. If a decision affects only other implementation
work, finish independent work and use the normal `LOOM_COMPLETE` / `LOOM_NOOP`
(Loop) or valid `LOOM_TODO` payload (Todo). A decision record is not completion,
and a human answer does not automatically close implementation work.

{% if self.output_phase() == loom_protocol::output::Phase::Loop %}
For this source's own discovery-child decisions, report the complete inventory
and use `LOOM_WAITING` with the source still `open`. Do **not** add a native
parent-to-child blocking dependency or set the parent `blocked`: that makes
human disposition of its children impossible. The driver validates ownership
and current inventory, stages the decisions, and persists a separate canonical
decision hold. Do not write driver hold metadata, labels or journal files.
Answers release only that hold; reread the decision outcomes and perform the
required implementation before ordinary completion. Other native dependencies
and independent human/semantic/infra holds remain authoritative.

If only other work needs the decisions and this source meets its own acceptance,
close the source and report decisions before the normal completion terminal:

```text loom-example
LOOM_CLARIFY: {"decisions":["lm-decision.1","lm-decision.2"]}
LOOM_COMPLETE
```

Use Noop instead when that independent source produced an intentionally empty
diff; use neither success terminal when its own prerequisites remain unresolved.
{% endif %}

Do not ask for ratification of a clear recommended path. When the spec, code,
and research establish a safe preference, implement or file that plan directly.
Reserve decisions for genuinely unresolved viable choices with materially
different costs or risks you cannot adjudicate. Options name concrete costs,
not a fixed generic menu.

When the session itself cannot finish, use exactly one terminal on the final
non-empty line, instead of a success terminal:

- `LOOM_RETRY: {"reason":"<nonblank reason>"}` — A fresh dispatch is likely
  to succeed after an environmental failure or agent self-reset. Explain the
  failure honestly; do not disguise a semantic decision as a transient error.
- `LOOM_BLOCKED: {"reason":"<nonblank no-safe-options rationale>"}` — A genuine
  semantic dead end: retry is not expected to help and candidate resolutions
  cannot be safely enumerated. The reason must explain **why options cannot be
  safely framed**, not merely say that user input is needed. If viable options
  can be framed, persist dedicated decisions, report their references, and use
  the source's appropriate independent terminal instead.

**Discriminator.** Fresh dispatch likely to help? → Retry. Safe unresolved
options available? → dedicated decision records plus the actual source outcome.
Semantic dead end with no safe options? → Blocked with the no-options reason.
Never use a Clarify record as a terminal or combine two terminals. Reasons
belong inside the typed JSON object, not in preceding prose. Strict JSON permits
pretty objects and escaped string newlines; raw string newlines are not repaired.

```text loom-example
LOOM_RETRY: {"reason":"Workspace IO failed; a fresh dispatch can retry the read."}
```

```text loom-example
LOOM_BLOCKED: {
  "reason": "The required source evidence is irrecoverably absent; options would invent unsupported obligations."
}
```

**Worker phases only.** Review is inspection-only: it uses clarify-route finding
Options evidence, never direct decision-bead mutation or Clarify records.
Interactive Plan/Inbox resolve questions with the human in-turn and do not use
worker self-reports.

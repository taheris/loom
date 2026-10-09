## Decision Records and Self-Reports

Decision discovery is independent of the session's terminal outcome. Loop and
Todo report all currently discovered unresolved human decisions through
nonterminal records, never a bare `LOOM_CLARIFY` terminal:

```text
LOOM_CLARIFY: {"decisions":["<decision-bead-id>","<another-decision-id>"]}
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

{% include "partial/options_format.md" %}

Only work whose acceptance genuinely requires a decision gets a prerequisite:
`bd dep add <affected-work> <decision>`. Parentage alone is not a prerequisite.
If the source itself must wait, declare its active prerequisites, leave it open,
then end with `LOOM_WAITING`. If a decision affects only other implementation
work, finish independent work and use the normal `LOOM_COMPLETE` / `LOOM_NOOP`
(Loop) or valid `LOOM_TODO` payload (Todo). A decision record is not completion,
and a human answer does not automatically close implementation work.

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
Never use a Clarify record as a terminal or combine two terminals.

**Worker phases only.** Review is inspection-only: it uses clarify-route finding
Options evidence, never direct decision-bead mutation or Clarify records.
Interactive Plan/Inbox resolve questions with the human in-turn and do not use
worker self-reports.

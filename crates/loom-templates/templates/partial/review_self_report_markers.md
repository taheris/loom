## Review Self-Report Markers

Review is inspection-only. Do not mutate `bd` state or persist review
self-report state with bd commands. Completed inspection reports findings
through the streaming protocol above, paired with Concern, or Complete for
zero findings. These outcomes are only for when the walk itself cannot finish;
use exactly one final logical terminal, with no trailing commentary.

- `Retry` — A fresh dispatch is likely to succeed after an environmental failure
  such as inaccessible workspace state or transient IO. Carry the nonblank
  reason inside the JSON payload, never on a preceding line.
- Clarify-worthy decisions use a clarify-route finding with one canonical
  Options brief in `evidence`, not direct decision-bead writes or Clarify
  records. If inspection can finish, pair those findings with Concern. Do not
  duplicate the evidence as separate prose narration.
- `Blocked` — A genuine semantic dead end: retry will not help and candidate
  resolutions cannot be safely framed. Its nonblank JSON reason explains **why
  no safe options can be enumerated**, not merely that human input is needed.

```text loom-example
LOOM_RETRY: {"reason":"Workspace IO interrupted inspection; a fresh dispatch can read the evidence."}
```

```text loom-example
LOOM_BLOCKED: {
  "reason": "The contractual source cannot be recovered; any proposed option would invent the intended invariant."
}
```

**Discriminator.** Fresh dispatch likely to help? → Retry. Viable unresolved
options? → clarify-route finding, then the actual review outcome. No safe
options? → Blocked with its typed rationale. Strict JSON accepts pretty objects
and escaped evidence newlines, never raw newline repair or preceding prose as a
reason/question payload. Loop/Todo use worker decision records; interactive
Plan/Inbox resolve questions with the human in-turn instead of self-reports.

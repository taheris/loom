## Scratchpad

You have a per-session scratchpad at `.wrapix/loom/scratch/<key>/scratch.md`,
where `<key>` is your session's concurrency unit — the spec label for `loom
plan` / `loom todo` sessions, the bead ID for `loom run` / `loom check` /
`loom msg` sessions. The file starts empty; append to it as the session
progresses.

**Purpose.** This is a **compaction-recovery aid, not durable storage.** The
file lives only for the lifetime of this session: the driver creates it at
session start and removes it at session end on every exit path. After
compaction, the harness re-pins the original prompt plus the scratchpad's
current contents so you regain the working notes you would otherwise have
lost.

**What to append:**

- Decisions you have made and the reasoning that tipped them, so you do not
  re-litigate them after compaction.
- Open questions you are tracking and what would resolve them.
- TODOs for the remainder of this session — files to touch next, checks to
  re-run, follow-ups inside this session's scope.
- Hypotheses you have already ruled out (with the evidence), so you do not
  re-explore them.

**What NOT to put here.** Anything that needs to outlive this session belongs
in a durable destination, not the scratchpad:

- Decisions, design rationale, or open questions that future sessions need →
  bead notes (`bd update <id> --notes "..."`) or the spec file (`specs/<label>.md`).
- Implementation context for future agents → spec file or `AGENTS.md` /
  companion docs.
- Why a change was made → the commit message.
- Cross-session follow-up work → a new bead (`bd create ...`).

The scratchpad will be deleted when this session ends. If a thought is worth
keeping past session end, write it to one of the durable destinations above
**before** you finish.

**How to append.** Append to the file directly — for example,
`echo "..." >> .wrapix/loom/scratch/<key>/scratch.md` or via the Edit tool.
Do not rewrite or truncate prior contents; the recovery payload is the full
running log.

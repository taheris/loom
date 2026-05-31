## Chat Discipline

Conduct this interactive session as a back-and-forth **chat** — natural
prose, not a structured questionnaire. Every interactive loom session
(`plan_new`, `plan_update`, `msg`) is conversational by design; the wrong
instrument (a multi-choice picker, a form-style prompt, a fixed enumeration)
collapses the user's real answer into a shape it doesn't fit.

- **Questions go out as assistant prose.** Ask one focused question per
  turn in your normal reply. Do not wrap the question in a separate
  picker UI, options panel, or interactive widget.
- **Answers come back as user prose.** Read whatever the user types —
  short ack, redirection, "none of the above", hybrid — and respond to
  intent, not to a slot.
- **Do NOT use Claude Code's structured option-picker tool** (the
  `AskUserQuestion` tool, or any equivalent multi-choice UI) during
  interactive sessions. The picker forces premature commitment to N
  enumerated options when the user's real answer may be a hybrid, a
  redirection, or none-of-the-above; it also adds friction to the short
  text replies that are the natural shape of conversational discussion.
- **Propose alternatives inline.** When you want to offer choices, list
  them in prose ("option A does X; option B does Y; or we could do
  Z"). The user replies "B" or "B with a tweak" or "neither, do Z" —
  natural prose, no picker UI.
- **Persistence destinations.** Session-bridging memory — decisions,
  context, follow-ups, anything future sessions need — goes into bd
  (`bd update <id> --notes …`, bead descriptions, or new beads via
  `bd create`) or spec files. bd persists across machines and after
  containers exit. Claude Code's `MEMORY.md` / auto-memory system is
  container-local and disappears with the container; treat it as
  working notes for the current session only, not as durable storage.
- **The "one by one" sub-mode is planning-specific.** When the planning
  interview is in that sub-mode, it means *one question per chat turn*,
  not *one picker per turn* — the chat-discipline rules above still
  apply within the sub-mode. Outside the planning templates the sub-mode
  is not in play.

Worker phases (`loop`, `todo_*`, `review`) are single-shot and do not
interview the user, so this partial is not pinned there.

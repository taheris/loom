## Chat Discipline

Conduct this planning interview as a back-and-forth **chat** — natural
prose, not a structured questionnaire. The interview is conversational
by design; the wrong instrument (a multi-choice picker, a form-style
prompt, a fixed enumeration) collapses the user's real answer into a
shape it doesn't fit.

- **Questions go out as assistant prose.** Ask one focused question per
  turn in your normal reply. Do not wrap the question in a separate
  picker UI, options panel, or interactive widget.
- **Answers come back as user prose.** Read whatever the user types —
  short ack, redirection, "none of the above", hybrid — and respond to
  intent, not to a slot.
- **Do NOT use Claude Code's structured option-picker tool** (the
  `AskUserQuestion` tool, or any equivalent multi-choice UI) during
  planning interviews. The picker forces premature commitment to N
  enumerated options when the user's real answer may be a hybrid, a
  redirection, or none-of-the-above; it also adds friction to the short
  text replies that are the natural shape of planning discussion.
- **Propose alternatives inline.** When you want to offer choices, list
  them in prose ("option A does X; option B does Y; or we could do
  Z"). The user replies "B" or "B with a tweak" or "neither, do Z" —
  natural prose, no picker UI.
- **The "one by one" sub-mode is preserved.** It means *one question
  per chat turn*, not *one picker per turn* — the discipline above
  still applies within the sub-mode.

This discipline is planning-specific. Worker phases (`run`, `todo_*`,
`review`) are single-shot and do not interview the user, so the
discipline does not apply there. `msg` resolves `loom:clarify` beads
via the canonical *Options Format Contract* in `specs/gate.md`, so the
picker concern doesn't apply there either.

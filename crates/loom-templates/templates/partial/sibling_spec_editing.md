## Anchor Context & Sibling-Spec Editing

Labels passed to `loom plan [SPEC_LABEL ...]` are **anchors**. They seed initial context only and do not define the touched set.

During this session you may read and edit **any spec in `specs/`** when a change cross-cuts sibling specs. No pre-declaration is required; the touched set emerges from the interview. `docs/README.md` is the spec index; consult it to locate siblings by name, label, and beads column.

**Creating a new sibling spec is also a valid outcome** when the planner judges that a section warrants its own spec. In that case create `specs/<label>.md` and record the new row in `docs/README.md`. Do not allocate a bead or epic during planning; `loom todo` creates spec/work epics later.

**Commits are not automatic.** Planning sessions edit specs in place but do **not** commit those edits. The agent saves the file(s), summarises what changed, and waits for the user to explicitly authorize the commit. Soft signals (*"looks good"*, *"next"*, *"accept"*) authorize the next interview step — not a commit. The commit happens only when the user uses unambiguous language (*"commit"*, *"ship it"*, *"land the changes"*, *"land the plane"*, *"push it"*). The same discipline applies to `git push`, `beads-push`, and any operation that mutates shared state — wait for the explicit trigger.

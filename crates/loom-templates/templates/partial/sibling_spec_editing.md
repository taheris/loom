## Anchor Session & Sibling-Spec Editing

The label named on the `-u` flag (`{{ label }}`) is the **anchor**; it owns the
session state row in the loom state database. The anchor's state holds the
molecule, `implementation_notes`, and `iteration_count` regardless of which
spec files are edited.

During this session you may read and edit **any spec in `specs/`** when a
change cross-cuts sibling specs. No pre-declaration is required — the touched
set emerges from the interview. `docs/README.md` is the spec index; consult it
to locate siblings by name, label, and beads column.

Rules:

- Edit sibling specs in place under `specs/` using the Edit tool, just like the
  anchor
- Sibling specs do **not** get their own state row or molecule during this
  session; `loom todo` creates sibling state rows lazily on fan-out

**Creating a new sibling spec is also a valid outcome** when the planner
judges that a section warrants its own spec. In that case the planner may
allocate a tracking epic for the new sibling (`bd create --type=epic
--title="..."`) and record its ID in `docs/README.md` (the spec index, so
the new spec has an index row from day one). This is the **one carve-out**
from the general "no bead creation during planning" rule — the epic is part
of the split's bookkeeping, not net-new implementation scoping.
Implementation beads for the new spec are created later, by `loom todo`,
once a future `loom plan -u <new-spec>` session has populated that spec.

**Commits are not automatic.** Planning sessions edit specs in place but do
**not** commit those edits. The agent saves the file(s), summarises what
changed, and waits for the user to explicitly authorize the commit. Soft
signals (*"looks good"*, *"next"*, *"accept"*) authorize the next interview
step — not a commit. The commit happens only when the user uses unambiguous
language (*"commit"*, *"ship it"*, *"land the changes"*, *"land the plane"*,
*"push it"*). This avoids premature commits that force iteration via
`git revert` or amend-rewrite. The same discipline applies to `git push`,
`beads-push`, and any operation that mutates shared state — wait for the
explicit trigger.

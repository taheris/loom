## Anchor Context & Sibling-Spec Editing

Labels passed to `loom plan [SPEC_LABEL ...]` are **anchors**. They seed initial context only and do not define the touched set.

During this session you may read and edit **any spec in `specs/`** when a change cross-cuts sibling specs. No pre-declaration is required; the touched set emerges from the interview. `docs/README.md` is the spec index; consult it to locate siblings by name, label, and beads column.

**Supported authoring layout.** Follow the index and the bootstrap cutover rule in `{{ spec_conventions }}`. After package-aware tooling is implemented and tested and the repository cuts over, each owner uses `specs/<label>/spec.md` for contracts and `specs/<label>/tests.md` for acceptance and verification strategy. Until cutover, preserve the entirely flat `specs/<label>.md` tree with inline Success Criteria and local section links. Do not mix layouts, silently relocate documents, or create a second acceptance copy. For touched siblings, read both package documents (or the complete flat owner) before editing.

**Creating a new sibling spec is also a valid outcome** when the planner judges that a section warrants its own spec. Create it in the repository's supported authoring layout and record the new row in `docs/README.md` in the same edit. Before finishing, verify every new owner has exactly one matching `docs/README.md` index row; unindexed specs are invisible to `loom todo`. Do not allocate a bead or epic during planning; `loom todo` creates spec/work epics later.

**Commits are not automatic.** Planning sessions edit specs in place but do **not** commit those edits. The agent saves the file(s), summarises what changed, and waits for the user to explicitly authorize the commit. Soft signals (_"looks good"_, _"next"_, _"accept"_) authorize the next interview step — not a commit. The commit happens only when the user uses unambiguous language (_"commit"_, _"ship it"_, _"land the changes"_, _"land the plane"_, _"push it"_). The same discipline applies to `git push`, `wrix beads push`, and any operation that mutates shared state — wait for the explicit trigger.

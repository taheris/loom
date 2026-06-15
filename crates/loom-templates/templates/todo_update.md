# Add Tasks to Existing Molecule

You are adding new tasks to an existing molecule. Your job is to identify new or
changed requirements and create tasks ONLY for those changes.

{% include "partial/context_pinning.md" %}

{% include "partial/spec_header.md" %}

## Anchor Spec

`{{ label }}` is the **anchor** for this session. The anchor spec below
(`specs/{{ label }}.md`) is shown for context — it's the spec that owns the
molecule, and it reflects what has already been implemented. Under
**Anchor-Driven Multi-Spec Planning**, a single `loom todo` run may also
create tasks for sibling specs in `specs/` that were touched during the plan
session; those sibling diffs appear in the Spec Diff section below.

Read the anchor spec at `{{ spec_path }}` for the full current contents before identifying changes.

{% include "partial/companions_context.md" %}

{% include "partial/scratchpad.md" %}
{% if !implementation_notes.is_empty() %}
## Implementation Notes

The following implementation notes were captured during planning. **Every
note below describes work that MUST become one or more beads in this
session.** They are the planning inputs to fan-out — not background
context to be appraised against the `criterion_status` surface. The
status cache reflects what prior molecules already shipped; each note
describes what the next molecule is supposed to land. A `Pass` row for
a criterion a note touches is not evidence that the note's work has
been done.

When this section is present, the session has **exactly two acceptable
exits**:

- **(a) Fan the notes into beads.** Every note becomes at least one
  bead bonded to this molecule. The `criterion_status` audit (next
  sections) and the existing-tasks list determine *how* notes split
  across beads, merge into a single bead, or order against existing
  work — they do not determine whether any given note is worked. Each
  bead receives the verbatim text of every note that informs it as
  part of its `--description`, so the implementation agent has the
  planning context independent of any external state.

- **(b) `LOOM_CLARIFY` on the molecule epic.** If you cannot tell
  whether the notes are still current — they predate landed work or
  existing tasks that appear to have already shipped them, they
  conflict with each other, or their scope is ambiguous — persist a
  `## Options — …` block to the molecule epic's notes per the
  *Options Format Contract* in `specs/gate.md` and emit
  `LOOM_CLARIFY`. The block must enumerate candidate interpretations
  (e.g. "treat note #N as already-covered by existing task lm-…" vs.
  "fan note #N into a follow-up bead anyway"). A subsequent `loom
  todo` run consumes the resolution before fanning out.

Emitting `LOOM_COMPLETE` or `LOOM_NOOP` with this section non-empty
and no new beads minted in this session is a **malformed exit**: the
notes named work, the session left without authoring it, and there is
no clarify on the molecule epic to capture the open question. The
verdict gate treats such a session as `zero-progress`.

{% for note in implementation_notes %}<implementation-note>
{{ note }}
</implementation-note>
{% endfor %}
{% endif %}
## Criterion Status

The status cache below shows the latest cached verifier verdict for each
Success-Criteria bullet in scope. Use these signals to distinguish criteria
already covered by a fresh-pass verifier from those that are stale, never-run,
or failing — they are the evidence the Decomposition Discipline (next section)
requires before you author any non-audit bead.

{% if criterion_status.is_empty() %}_No cached status rows for this spec. Treat this as the empty-cache case
described in the Decomposition Discipline below: every criterion arrives
without evidence, so either author beads only for confirmed gaps after
reading the relevant implementations, or emit `LOOM_CLARIFY` on the molecule
epic when the volume is too large to inline-audit._
{% else %}{% for row in criterion_status %}- **{{ row.criterion_anchor }}** · annotation `{{ row.annotation }}` · result `{{ row.last_result.as_str() }}` · last commit {% match row.last_commit %}{% when Some with (c) %}`{{ c }}`{% when None %}—{% endmatch %} · commits since {% match row.commits_since %}{% when Some with (n) %}{{ n }}{% when None %}—{% endmatch %} · last timestamp {% match row.last_timestamp_ms %}{% when Some with (t) %}{{ t }}{% when None %}—{% endmatch %}
{% endfor %}{% endif %}
{% include "partial/decomposition_discipline.md" %}

## Spec Changes

If `spec_diff` is provided, use that to identify new requirements **across all
listed specs**; otherwise use `existing_tasks` to compare against the anchor
spec and identify gaps.

Under **per-spec cursor fan-out**, `spec_diff` may span multiple sibling specs.
Each diff block is prefixed with its spec path in the form `=== specs/<s>.md ===`
so you can attribute every new requirement to the correct spec file.

### Spec Diff (git-based)

```diff
{% match spec_diff %}{% when Some with (diff) %}{{ diff }}{% when None %}{% endmatch %}
```

### Existing Tasks

<agent-output>
{% match existing_tasks %}{% when Some with (tasks) %}{{ tasks }}{% when None %}{% endmatch %}
</agent-output>

## Existing Molecule

Molecule ID: {% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}—{% endmatch %}

Use `bd mol show {% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}<molecule>{% endmatch %}` to see the current tasks in this molecule.
All new tasks in this session — whether they implement the anchor or a sibling
spec — bond to this molecule. Sibling specs do NOT get their own molecule.

## Instructions

1. **Identify changes** — If `spec_diff` is non-empty, focus on the added/changed
   lines in the diff across every spec listed under its `=== specs/<s>.md ===`
   headers. If `spec_diff` is empty, compare the full anchor spec against
   `existing_tasks` to find requirements that lack corresponding tasks.
2. **Create new tasks as children of the anchor's molecule**, labelling each
   task with the spec it implements (may differ from the anchor under fan-out):
   ```bash
   # <s> is the spec file the task implements, e.g. the anchor ({{ label }}) or
   # a sibling label derived from specs/<sibling>.md.
   TASK_ID=$(bd create --title="<task title>" --description="<detailed description>" \
     --type=task --labels="spec:<s>,profile:<profile>" --parent="{% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}<molecule>{% endmatch %}" --silent)
   ```
3. **Assign profile per-task** based on implementation needs:
   - Tasks touching `.rs` files or using cargo → `profile:rust`
   - Tasks touching `.py` files or using pytest/pip → `profile:python`
   - Tasks touching only `.nix`, `.sh`, `.md` files → `profile:base`
4. **Set execution order** with `bd dep add` if new tasks depend on existing ones:
   ```bash
   bd dep add <new-task> <existing-task>  # new-task waits for existing-task
   ```
5. **Do NOT create tasks** for requirements that already have corresponding tasks
   in the molecule

### Key Concepts

| Mechanism | Purpose | Effect |
|-----------|---------|--------|
| `--parent` | Links task to the anchor's molecule | Enables `loom status` progress tracking |
| `bd dep add` | Sets execution order | Controls what `bd ready` returns next |
| `spec:<s>` | Marks which spec file a task implements | Attributes tasks to anchor vs sibling under fan-out |
| `profile:X` | Selects container profile | Determines toolchain available in `loom loop` |

## Task Breakdown Guidelines

- Each task should be **self-contained** with enough context for a fresh agent
- Consider dependencies on **existing tasks** in the molecule
- Keep tasks **focused** - one clear objective per task
- Include **test tasks** where appropriate
- **Assign profile per-task** based on what that specific task needs
- **Label each task with `spec:<s>`** identifying which spec file it implements
  — under fan-out, `<s>` may be the anchor label (`{{ label }}`) or a sibling label

## Spec Index Backfill (anchor only)

After creating tasks, check if the pinned-context file (the project spec index
shown above under "Context Pinning" — `docs/README.md` by default) has an empty
Beads column for the **anchor** spec (`{{ label }}`). If the Beads column is
empty, fill in the molecule ID (`{% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}<molecule>{% endmatch %}`).

Sibling specs touched in this session do **not** get their own molecule — their
tasks bond to the anchor's molecule. Do **NOT** write the anchor's molecule ID
into any sibling's README Beads column; a sibling's row stays empty until it is
planned as its own anchor in a future `loom plan <sibling>` session.

{% include "partial/progress_markers.md" %}

{% include "partial/self_report_markers.md" %}

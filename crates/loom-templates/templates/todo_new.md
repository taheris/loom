# Task Decomposition

You are decomposing a specification into implementable tasks. Your goal is to
create a beads molecule (epic + child issues) that breaks down the work.

{% include "partial/context_pinning.md" %}

{% include "partial/spec_header.md" %}

## Specification Content

Read the spec at `{{ spec_path }}` for full content before decomposing.

{% include "partial/companions_context.md" %}

{% include "partial/scratchpad.md" %}
{% if !implementation_notes.is_empty() %}
## Implementation Notes

The following implementation notes were captured during planning. **Every
note below describes work that MUST become one or more beads in this
session.** They are the planning inputs to fan-out — not background
context to be appraised against the `criterion_status` surface. The
status cache reflects what the *previous* molecule already shipped; each
note describes what the *next* molecule is supposed to land. A `Pass`
row for a criterion a note touches is not evidence that the note's work
has been done.

When this section is present, the session has **exactly two acceptable
exits**:

- **(a) Fan the notes into beads.** Every note becomes at least one
  bead in this session's molecule. The `criterion_status` audit (next
  sections) determines *how* notes split across beads, merge into a
  single bead, or order against each other — it does not determine
  whether any given note is worked. Each bead receives the verbatim
  text of every note that informs it as part of its `--description`,
  so the implementation agent has the planning context independent of
  any external state.

- **(b) `LOOM_CLARIFY` on the molecule epic.** If you cannot tell
  whether the notes are still current — they predate landed work that
  appears to have already shipped them, they conflict with each other,
  or their scope is ambiguous — persist a `## Options — …` block to
  the molecule epic's notes per the *Options Format Contract* in
  `specs/gate.md` and emit `LOOM_CLARIFY`. The block must enumerate
  candidate interpretations (e.g. "treat note #N as already-landed
  and skip it" vs. "fan note #N into a verification bead anyway"). A
  subsequent `loom todo` run consumes the resolution before fanning
  out.

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

{% if criterion_status.is_empty() %}_No parsed criteria were available for this spec. Treat this as a preflight problem, not as evidence of no work._
{% else %}{% for row in criterion_status %}- **{{ row.criterion_id }}** · {{ row.criterion_text }} · annotation `{{ row.annotation }}` · evidence `{{ row.evidence.as_str() }}` · result {{ row.evidence.result_label() }} · last commit {{ row.evidence.last_commit_label() }} · commits since {{ row.evidence.commits_since_label() }} · last timestamp {{ row.evidence.last_timestamp_label() }} · cached annotation `{{ row.evidence.cached_annotation_label() }}`
{% endfor %}{% endif %}
{% include "partial/decomposition_discipline.md" %}

## Task Breakdown Guidelines

- Each task should be **self-contained** with enough context for a fresh agent
- Order tasks by **dependencies** (what must be done first)
- Keep tasks **focused** - one clear objective per task
- Include **test tasks** where appropriate
- **Assign profile per-task** based on what that specific task needs

## Profile Assignment

Each task needs a `profile:X` label to select the right container toolchain in `loom loop`:

| Task Type | Profile | When to Use |
|-----------|---------|-------------|
| Rust implementation | `profile:rust` | Tasks touching `.rs` files or using cargo |
| Python implementation | `profile:python` | Tasks touching `.py` files or using pytest/pip |
| Nix/shell/docs | `profile:base` | Tasks touching only `.nix`, `.sh`, `.md` files |

Different tasks in the same molecule can have different profiles. Assign based on what each specific task needs.

## Instructions

1. **Analyze the spec** - Understand all requirements and affected files
2. **Create the epic** (molecule root) and **store its ID**:
   ```bash
   MOLECULE_ID=$(bd create --type=epic --title="<feature name>" --labels="spec:{{ label }}" --silent)
   echo "Created molecule: $MOLECULE_ID"
   ```
   **CRITICAL:** Use the exact ID returned by `bd create --silent`. Do NOT substitute
   a molecule ID from the spec index or any other source — `bd create` generates a
   unique ID and that is the only valid value. Create the epic **before** any
   criterion-by-criterion gap analysis below, so the `LOOM_CLARIFY`-on-epic
   fallback always has a valid target if the audit cannot complete.
3. **Audit before fan-out** — walk every Success-Criteria bullet in scope
   against the **Criterion Status** section above. A row whose evidence is
   `Current`, whose result is `Pass`, and whose commits-since count is low
   is positive evidence of coverage — do **not** author a bead for it. For
   every suspicious row (`Missing`, `StaleAnnotation`, failed/skipped
   current evidence, large commits-since, or a verifier-target name whose body may not
   exercise the live system), read the verifier function and the
   production code path it claims to cover before deciding the criterion
   is a gap. Cite the evidence (the criterion-status row, the file read,
   and/or the verifier-source observation) in each bead's description so
   the review walk can distinguish gap-targeted beads from axis-fan-out
   beads. If coverage cannot be determined by inspection, persist a
   `## Options — …` block to the molecule epic's notes and emit
   `LOOM_CLARIFY` per the Decomposition Discipline above.
4. **Create child tasks** with `--parent` and appropriate `profile:X` label:
   ```bash
   TASK_ID=$(bd create --title="<task title>" --description="<detailed description>" \
     --type=task --labels="spec:{{ label }},profile:rust" --parent="$MOLECULE_ID" --silent)
   ```
5. **Set execution order** with `bd dep add` for tasks that must run sequentially:
   ```bash
   bd dep add <later-task> <earlier-task>  # later-task waits for earlier-task
   ```

### Key Concepts

| Mechanism | Purpose | Effect |
|-----------|---------|--------|
| `--parent` | Links task to molecule | Enables `loom status` progress tracking |
| `bd dep add` | Sets execution order | Controls what `bd ready` returns next |
| `profile:X` | Selects container profile | Determines toolchain available in `loom loop` |

Both `--parent` and `bd dep add` are required: `--parent` for visibility, `bd dep add` for ordering.

## Output Format

After creating all tasks:

1. List the epic ID and all task IDs created
2. Show the dependency graph
3. Confirm the molecule was created

{% include "partial/progress_markers.md" %}

{% include "partial/self_report_markers.md" %}

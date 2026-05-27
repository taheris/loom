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

The following implementation notes were captured during planning. They carry
hidden constraints, file paths, and design context that every implementation
agent must see. **Copy every note's text verbatim into the `--description` of
every bead you create in this session**, so each implementation agent receives
the full context independent of any external state:

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
   against the **Criterion Status** section above. A row whose verdict is
   `Pass`, whose timestamp is recent, and whose commits-since count is low
   is positive evidence of coverage — do **not** author a bead for it. For
   every suspicious row (stale timestamp, large commits-since, `NoResult`
   on a fresh checkout, or a verifier-target name whose body may not
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

{% include "partial/exit_signals.md" %}

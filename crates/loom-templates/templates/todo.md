# Todo Decomposition

You are decomposing the driver-injected changed-spec roster into bead work. The driver has already created or reused the `loom:todo` work epic for this batch; do not create a molecule epic yourself.

{% include "partial/context_pinning.md" %}

## Spec Index

{{ spec_index }}

## Todo Batch

- **Work epic**: {{ work_epic }}
- **Todo head**: {{ todo_head }}
- **Todo fingerprint**: {{ todo_fingerprint }}

## Changed Specs

Decompose exactly the specs listed here. Every label in this roster must appear exactly once in the final `LOOM_TODO:` payload.

{% if changed_specs.is_empty() %}_No changed specs were injected. Treat this as a preflight problem and self-report instead of emitting success._
{% else %}{% for spec in changed_specs %}### {{ spec.label }}

- Spec file: `{{ spec.spec_path }}`
{% match spec.diff %}{% when Some with (diff) %}
Diff:

```diff
{{ diff }}
```
{% when None %}- Diff: not provided; read the spec file and use the criterion-status evidence below.
{% endmatch %}
{% endfor %}{% endif %}
## Spec Epic Metadata

{% if spec_epics.is_empty() %}_No cached spec epic metadata was injected._
{% else %}{% for epic in spec_epics %}- **{{ epic.label }}** · epic {% match epic.epic_id %}{% when Some with (id) %}{{ id }}{% when None %}—{% endmatch %} · todo cursor {% match epic.todo_cursor %}{% when Some with (cursor) %}`{{ cursor }}`{% when None %}—{% endmatch %}
{% endfor %}{% endif %}
{% include "partial/companions_context.md" %}

{% include "partial/scratchpad.md" %}
{% include "partial/skill_index.md" %}{% if self.has_implementation_notes() %}
## Implementation Notes

The following implementation notes were captured during planning. **Every note below describes work that MUST become one or more beads in this session** unless you persist a structured clarify question to the work epic and self-report.

{% for group in implementation_notes %}{% if !group.notes.is_empty() %}### {{ group.label }}

{% for note in group.notes %}<implementation-note>
<agent-output>
{{ note }}
</agent-output>
</implementation-note>
{% endfor %}{% endif %}{% endfor %}{% endif %}
## Criterion Status

The status cache below shows the latest cached verifier verdict for each Success-Criteria bullet in scope. Use these typed rows to distinguish criteria already covered by fresh positive evidence from stale, missing, failed, or skipped evidence.

{% if criterion_status.is_empty() %}_No parsed criterion-status rows were injected. Treat this as a preflight problem, not as evidence of no work._
{% else %}{% for row in criterion_status %}- **{{ row.spec_label }} / {{ row.criterion_id }}** · {{ row.criterion_text }} · annotation `{{ row.annotation }}` · evidence `{{ row.evidence.as_str() }}` · result {{ row.evidence.result_label() }} · last commit {{ row.evidence.last_commit_label() }} · commits since {{ row.evidence.commits_since_label() }} · last timestamp {{ row.evidence.last_timestamp_label() }} · cached annotation `{{ row.evidence.cached_annotation_label() }}`
{% endfor %}{% endif %}
{% include "partial/decomposition_discipline.md" %}

## Task Breakdown Guidelines

- Create implementation beads only as children of the injected work epic `{{ work_epic }}`.
- Label every bead with `spec:<label>` for the changed spec it implements and the appropriate `profile:<name>` label.
- Keep each bead self-contained with enough context for a fresh `loom loop` agent.
- Set dependencies with `bd dep add` when one bead must land before another.
- Include the criterion-status row or source-inspection evidence that justified each non-audit bead.

## Profile Assignment

| Task Type | Profile | When to Use |
|-----------|---------|-------------|
| Rust implementation | `profile:rust` | Tasks touching `.rs` files or using cargo |
| Python implementation | `profile:python` | Tasks touching `.py` files or using pytest/pip |
| Nix/shell/docs | `profile:base` | Tasks touching only `.nix`, `.sh`, `.md` files |

## Instructions

1. Use the injected changed-spec roster, diffs, implementation notes, and Criterion Status rows as the starting evidence. Do **not** perform a blanket full-file read of every changed spec; read only targeted spec sections, companion manifests, representative code, or verifier bodies needed to resolve an actual ambiguity.
2. Audit the Criterion Status rows before fan-out. `EvidenceState::Missing` and `EvidenceState::StaleAnnotation` are absence or staleness of evidence; inspect representative code and verifier bodies before creating non-audit beads.
3. Create needed beads under the driver-created work epic:
   ```bash
   TASK_ID=$(bd create --title="<task title>" --description="<detailed description>" \
     --type=task --labels="spec:<label>,profile:<profile>" --parent="{{ work_epic }}" --silent)
   ```
4. Set execution order with `bd dep add <later-task> <earlier-task>` where required.
5. When finished, stop using tools and emit success only through the typed todo payload described below as your final assistant message. Do not use `bash`, `python`, `echo`, `printf`, or any other tool to print the terminal line.

{% include "partial/options_format.md" %}

{% include "partial/self_report_markers.md" %}

{% include "partial/todo_success.md" %}

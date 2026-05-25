# Specification Update Interview

You are refining an existing specification. Your goal is to gather additional
requirements and update the spec file directly.

**IMPORTANT: This is a planning-only phase. Do NOT write or modify any code. Your role is to discuss and update the specification only.**

{% include "partial/context_pinning.md" %}

{% include "partial/spec_header.md" %}

## Existing Specification

Read the existing spec at `{{ spec_path }}` for full context before refining.

{% include "partial/companions_context.md" %}

{% include "partial/spec_conventions.md" %}

{% include "partial/scratchpad.md" %}

## Existing Implementation Notes

The following implementation notes are currently attached to this spec (the
`notes` rows where `kind = 'implementation'`). They are transient context
seeded by the prior `loom plan` and consumed by `loom todo`. **Review them
in light of the changes you are about to make and rewrite the set as a
merge**, not a blind append and not a blind replace:

- **Keep** notes whose hint or constraint still holds after this update.
- **Drop** notes that the new decisions invalidate or supersede.
- **Add** fresh notes that capture hidden constraints, file paths, or
  trade-offs the new requirements introduce.

{% if implementation_notes.is_empty() %}
_(no implementation notes are currently attached to this spec)_
{% else %}{% for note in implementation_notes %}<implementation-note>
{{ note }}
</implementation-note>
{% endfor %}{% endif %}

Before exiting, write the **merged** array back via:

```bash
loom note set {{ label }} --kind implementation --json '["merged note 1", …]'
```

`loom note set` is atomic: it replaces every `kind = implementation` row
for `{{ label }}` with the supplied array in a single transaction. Pass the
full merged array, not a delta — `set` is by-design destructive against the
prior set. Pass `'[]'` if the merge result is empty (every prior note was
dropped and no new note belongs).

## Update Guidelines

1. **Discuss NEW requirements only** - The existing spec has been implemented
2. **Ask clarifying questions** to understand the additional work needed
3. **Capture scope clearly** - What new functionality is being added?

{% include "partial/sibling_spec_editing.md" %}

## Interview Flow

1. Ask the user what additional work they want to add
2. Clarify the new requirements:
   - What problem does the new work solve?
   - How does it relate to existing functionality?
   - What are the success criteria for the new work?
   - Does the change cross-cut any sibling specs in `specs/`?
3. Before applying a change to the spec, run the **Plan-Stage Rubric**
   described below — completeness, internal coherence, and invariant-clash
   checks against the anchor **and any touched sibling specs**. Pause the
   interview if any check flags.
4. When requirements are clear and no unresolved flags remain, edit
   `specs/{{ label }}.md` (anchor) and any touched sibling specs directly to
   integrate the new requirements into the appropriate sections
5. Do not `git add`. Do not `git commit`. Leave the edits as modified
   files in the working tree.
6. The session continues until the user gives an **explicit instruction
   to commit / close the session**. "Land the plane" is this project's
   canonical phrase (see `AGENTS.md`) for the full session-close flow;
   `commit it` and `push it` work too. The trigger must name the action.
7. Acknowledgements ("ok", "yes", "looks good", "sounds right", "go
   ahead", "done") are agreement to whatever was just discussed — they
   are NOT commit triggers, even when the prior turn was about
   committing. If unclear, ask "Ready to land the plane?" and wait.
8. On an explicit trigger, run the full session-close flow per
   `AGENTS.md`: stage the anchor and every touched sibling spec,
   commit, push, run `beads-push`, then output LOOM_COMPLETE.

{% include "partial/plan_stage_rubric.md" %}

## Spec Editing

When updating the spec, use the Edit tool to modify `specs/{{ label }}.md`
(anchor) and any touched sibling specs under `specs/` directly:

1. Determine where new content belongs:
   - If it updates an existing section → **edit that section in place**
   - If it adds a new capability → **add a new section in the appropriate location**
   - If it supersedes existing content → **replace the old content**
   - If the change cross-cuts, place each piece in the spec it belongs to
     (anchor or sibling) rather than duplicating
2. Keep each spec **concise** — every spec remains a single source of truth,
   not a changelog
3. After editing, leave the files as modified in the working tree — do
   not `git add`, do not `git commit`. `loom todo` detects them via
   per-spec cursor fan-out once the user commits.

`loom todo` will then:
1. Detect spec changes via `git diff` against each spec's `base_commit`
   (per-spec cursor fan-out, bounded by the anchor's cursor)
2. Create tasks ONLY for new/changed requirements across the anchor and any
   touched siblings, bonded to the anchor's molecule
3. Advance `base_commit` on every spec that received tasks

{% include "partial/interview_modes.md" %}

{% include "partial/exit_signals.md" %}

{% include "partial/chat_marker_final_turn_only.md" %}

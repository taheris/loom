# Implementation Step

{% include "partial/context_pinning.md" %}

{% include "partial/spec_header.md" %}

{% include "partial/companions_context.md" %}

{% include "partial/style_rules.md" %}

{% include "partial/scratchpad.md" %}

## Current Spec

Read: {{ spec_path }}

## Issue Details

Issue: {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}—{% endmatch %}
Title: <agent-output>{% match title %}{% when Some with (t) %}{{ t }}{% when None %}{% endmatch %}</agent-output>

<agent-output>
{% match description %}{% when Some with (d) %}{{ d }}{% when None %}{% endmatch %}
</agent-output>

{% match previous_failure %}{% when Some with (failure) %}{% if attempt > 0 %}Retry attempt {{ attempt }} — previous attempt failed with:

{% endif %}<agent-output>
{{ failure }}
</agent-output>{% match review_notes %}{% when Some with (notes) %}

Review notes:
<agent-output>
{{ notes }}
</agent-output>{% when None %}{% endmatch %}{% when None %}{% endmatch %}

## Instructions

{% if previous_failure.is_some() && attempt > 0 %}> Re-read the previous failure block above and address its specific
> concern before re-implementing.

{% endif %}1. **Understand**: Read the spec and issue thoroughly before making changes
2. **Test Strategy**: Decide between:
   - Property-based tests: For functions with clear invariants, mathematical properties
   - Unit tests: For specific behaviors, edge cases, integration points
3. **Implement**: Write code following the spec
4. **Discovered Work**: If you find tasks outside this issue's scope:
   - Create the issue as a child of the molecule:
     ```bash
     NEW_ID=$(bd create --title="..." --type=task --labels="spec:{{ label }}" \
       --parent="{% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}<molecule>{% endmatch %}" --silent)
     ```
   - Set execution order if needed:
     - **Blocks current task**: `bd dep add {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue>{% endmatch %} $NEW_ID` (current waits for new)
     - **Depends on current task**: `bd dep add $NEW_ID {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue>{% endmatch %}` (new waits for current)
     - **Independent**: No dep needed—`bd ready` will surface it when unblocked
   - Do NOT implement discovered tasks in this session—stay focused
5. **Quality Gates**: Before completing, ensure:
   - All tests pass
   - Lint checks pass
   - Changes committed
6. **Blocked vs Waiting**: Distinguish dependency blocks from true blocks:
   - Need user input? → write the reason on a prior line, then `LOOM_BLOCKED`
   - Need other beads done? → Add dep with `bd dep add`, output `LOOM_COMPLETE`
7. **Already Implemented**: If the task's work is already done in the codebase,
   verify correctness, run `bd close {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue-id>{% endmatch %}`,
   and emit `LOOM_NOOP` (empty diff means no-op, not zero-progress).
8. **Closing the bead**: After committing your changes and before emitting
   `LOOM_COMPLETE`, run `bd close {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue-id>{% endmatch %}`.
   The verdict gate observes `bd-closed` as the agent's responsibility — a
   `LOOM_COMPLETE` without a closed bead is treated as `incomplete-signaling`
   and enters recovery.

## Spec Verifications

After implementing the issue, check the spec's Success Criteria for `[verify]` and
`[judge]` annotations that reference test files relevant to this issue's work.

- **`[verify]` tests**: Create the referenced shell test file if it doesn't exist.
  The test function should exercise the implemented feature and exit 0 on success,
  non-zero on failure. Use `set -euo pipefail`. Exit 77 to skip (e.g. platform
  not available).
- **`[judge]` tests**: Create the referenced judge rubric file if it doesn't exist.
  Each function calls `judge_files "path/to/source"` and
  `judge_criterion "what to evaluate"`. See `tests/judges/example.sh` for format.
- Only create tests for criteria related to the current issue — don't implement
  all spec verifications, just the ones relevant to your work.
- If the test file already exists, add your function to it rather than overwriting.

## Quality Gates

Before outputting LOOM_COMPLETE:
- Tests written and passing
- Lint checks pass
- Changes staged (`git add`)
- Spec verification test files created for relevant criteria
- Bead closed (`bd close {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue-id>{% endmatch %}`)

Post-step hooks verify compliance automatically.

## Land the Plane

Before outputting LOOM_COMPLETE, follow the **Session Protocol** in `AGENTS.md`.

{% include "partial/exit_signals.md" %}

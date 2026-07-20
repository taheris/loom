# Implementation Step

{% include "partial/context_pinning.md" %}

{% include "partial/spec_header.md" %}

{% include "partial/companions_context.md" %}

{% include "partial/style_rules.md" %}

{% include "partial/scratchpad.md" %}
{% include "partial/skill_index.md" %}
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
</agent-output>{% when None %}{% endmatch %}{% when None %}{% endmatch %}{% include "partial/workspace_recovery.md" %}

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
     - **Blocks current task**: `bd dep add {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue>{% endmatch %} $NEW_ID` (current waits for new), then emit `LOOM_WAITING`
     - **Depends on current task**: `bd dep add $NEW_ID {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue>{% endmatch %}` (new waits for current)
     - **Independent**: No dep needed—`bd ready` will surface it when unblocked
   - Do NOT implement discovered tasks in this session—stay focused
5. **Quality Gates**: Before completing, ensure:
   - All tests pass
   - Lint checks pass
   - Changes committed
6. **Blocked vs Waiting**: Distinguish dependency waits, clarify decisions, and true dead ends:
   - Need user input and can frame candidate options? → persist the Options block, then `LOOM_CLARIFY`
   - Semantic dead end with no safe options to enumerate? → write that no-options rationale on a prior line, then `LOOM_BLOCKED`
   - Need other beads done? → Add the blocking dependency with `bd dep add`, leave this bead open, then output `LOOM_WAITING`
7. **Already Implemented**: If the task's work is already done in the codebase,
   verify correctness, run `bd close {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue-id>{% endmatch %}`,
   and emit `LOOM_NOOP` (empty diff means no-op, not zero-progress).
8. **Closing the bead**: After committing your changes and before emitting
   `LOOM_COMPLETE`, run `bd close {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue-id>{% endmatch %}`.
   The verdict gate observes `bd-closed` as the agent's responsibility — a
   `LOOM_COMPLETE` without a closed bead is treated as `incomplete-signaling`
   and enters recovery.

## Spec Verifications

After implementing the issue, check the spec's Success Criteria for
`[check]` / `[test]` / `[system]` / `[judge]` annotations whose targets
are relevant to this issue's work.

- **`[check]` annotations**: The target is a shell command (e.g.
  `cargo run -p loom-walk -- foo`) — no file to create. If the command's
  first token doesn't yet exist on PATH or the walk it names isn't yet
  wired, land that wiring in this issue.
- **`[test]` annotations**: The target is a language-native path. For
  Rust (`crate::module::test_name`), add the `#[test]` function in the
  matching module. For shell (`tests/foo.sh#test_x`), create the
  referenced file if it doesn't exist; the function exits 0 on success,
  non-zero on failure, uses `set -euo pipefail`, and exits 77 to skip
  (e.g. platform not available).
- **`[system]` annotations**: The target is a system-level command
  (`nix run .#smoke`, container build). Same rules as `[check]` —
  it's a command string, not a file to create. Make sure the system
  invocation actually exists.
- **`[judge]` annotations**: Create the referenced judge rubric file if
  it doesn't exist. Each function calls `judge_files "path/to/source"`
  and `judge_criterion "what to evaluate"`. See `tests/judges/example.sh`
  for format.
- Only create verifiers for criteria related to the current issue — don't
  implement all spec verifications, just the ones relevant to your work.
- If the test file already exists, add your function to it rather than
  overwriting.

## Quality Gates

Before outputting LOOM_COMPLETE:
- Tests written and passing
- Lint checks pass
- Changes staged (`git add`)
- Spec verification test files created for relevant criteria
- Bead closed (`bd close {% match issue_id %}{% when Some with (id) %}{{ id }}{% when None %}<issue-id>{% endmatch %}`)
- Preflight self-check: run `loom gate verify --diff <bead-base>..HEAD` using the exact bead base injected for this dispatch. If the branch upstream is that injected base, `loom gate verify --diff @{u}..HEAD` is acceptable. Resolve any findings in-session before emitting `LOOM_COMPLETE` — do not defer findings to a follow-up bead.
- Rerun the self-check after any later commit, formatter or hook tree change, or other change that could invalidate the prior run.
- Prompt-level self-review: before the final marker, re-read the issue criteria, inspect the committed diff, verify style/spec fit, and either fix issues or emit `LOOM_RETRY`, `LOOM_CLARIFY`, or `LOOM_BLOCKED` with the required self-report framing.

Post-step hooks verify compliance automatically.

## Land the Plane

Before outputting LOOM_COMPLETE, follow the **Session Protocol** in `AGENTS.md`.

{% include "partial/dependency_wait.md" %}

{% include "partial/progress_markers.md" %}

{% include "partial/self_report_markers.md" %}

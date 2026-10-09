set -euo pipefail

# Agent-output routing

    judge_agent_output_routing() {
      judge_files \
        "crates/loom-templates/templates/loop.md" \
        "crates/loom-templates/templates/todo.md" \
        "crates/loom-templates/templates/review.md" \
        "crates/loom-templates/templates/partial/self_report_markers.md" \
        "crates/loom-templates/templates/partial/review_self_report_markers.md" \
        "crates/loom-templates/templates/partial/decomposition_discipline.md" \
        "crates/loom-templates/templates/partial/workspace_recovery.md" \
        "crates/loom-templates/src/previous_failure.rs" \
        "crates/loom-templates/templates/partial/findings_walk.md" \
        "crates/loom-workflow/src/review/production.rs" \
        "crates/loom-workflow/src/review/phase_verdict.rs"
      judge_criterion \
        "Trace the actual rendered Loop, Todo and inspection Review guidance through all included partials. Blocked is only a genuine semantic dead end whose nonblank reason explains why safe options cannot be framed; needing human input alone is not Blocked. Loop/Todo persist one dedicated decision bead and Options brief per unresolved question, stage candidates outside queues, report all known references through nonterminal LOOM_CLARIFY decisions records, and independently select their source terminal. Decisions affecting only other work permit independent completion; actual source prerequisites require explicit edges and Waiting with an open source. Todo may finalize a valid roster despite implementation-only decisions, but decomposition prerequisites wait. Parentage is provenance, not a blanket dependency, and an answer does not automatically close work. Inspection review never creates or mutates decision beads or emits direct Clarify records: frameable choices use ordinary clarify-route LOOM_FINDING records with canonical Options in evidence and LOOM_CONCERN. Judge behavior for a worker that finishes while two choices block other tasks, a Todo roster with implementation-only choices, a truly blocked decomposition, and a review invariant clash with two viable resolutions. Fail on any contradictory included instruction, source-level multi-question brief, bare Clarify terminal, missing no-options rationale, or reviewer Beads mutation authority. This evaluates delivered guidance and ordinary finding routing, not a new decision-admission implementation or extra gate."
    }

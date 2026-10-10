set -euo pipefail

# Package contract coverage

    judge_package_contract_coverage() {
      judge_files \
        "docs/spec-conventions.md" \
        "docs/style-rules.md" \
        "crates/loom-workflow/src/plan/prompt.rs" \
        "crates/loom-workflow/src/plan/runner.rs" \
        "crates/loom-workflow/tests/planning_guidance.rs" \
        "crates/loom-templates/templates/plan.md" \
        "crates/loom-templates/templates/partial/plan_stage_rubric.md" \
        "crates/loom-templates/templates/review.md" \
        "crates/loom-templates/templates/partial/review_rubric.md" \
        "crates/loom-templates/tests/render.rs" \
        "crates/loom-templates/tests/snapshots/snapshots__plan_snapshot.snap" \
        "crates/loom-templates/tests/snapshots/snapshots__review_snapshot.snap" \
        "tests/fixtures/planning/coverage/*/*.md" \
        "tests/fixtures/planning/coverage-traces.md"
      judge_criterion \
        "Follow the actual Plan and Review prompt assembly/include graph and their rendered outputs, not keyword counts. Evaluate TST-5 semantic coverage across the owning spec.md and tests.md: every behavioral contract section, normative prose claim and lifecycle/decision/contract row must map via ordinary Markdown section links to criteria that capture all asserted behavior. Exactly one adjacent verifier belongs to each criterion in tests.md, not duplicated in spec.md. Section-level links may cover multiple rows; require neither per-row repeated links nor a registry nor equal row/annotation counts. Verify the concrete native fixture observations and independently judge each proposed continuation: unmapped duplicate-admission prose and both cancellation rows must be rejected; the two irrelevant equal-count criteria must be rejected despite valid links; the split package with a single Cancellation link/criterion covering both rows must be accepted without annotation duplication. Structural parser success is not semantic proof and prospective bindings are not execution evidence. The delivered planning guidance must distinguish honest binary/assertion pending from provider admission errors, required host capability gaps, skips/timeouts/incomplete work, keep readiness in Verify's shared admitted planner, forbid laundering those failures with ?, preserve ordinary sibling coverage during sharing, and require same-diff marker removal on resolution (including structured ?/~ cells). Preserve entirely flat bootstrap authoring and local links until tested tooling and repository cutover; do not accept missing package documents as a new flat owner. Fail if delivery omits guidance, accepts an uncovered behavior, substitutes valid links/counts for meaning, demands row-level duplication, or excuses arbitrary spawn errors. Identify concrete missing behavior and the delivered instruction responsible. Scripted semantic traces are probes, not proof that a model performed the assessment; do not claim runtime interview success from fixture expectations alone. Review coverage failures use existing finding targets/routes, not a new gate."
    }

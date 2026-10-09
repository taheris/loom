set -euo pipefail

# Coverage reductions

    judge_coverage_reductions() {
      judge_files \
        "crates/loom-workflow/src/review/production.rs" \
        "crates/loom-workflow/src/review/inspection.rs" \
        "crates/loom-templates/templates/review.md" \
        "crates/loom-templates/templates/partial/review_rubric.md" \
        "crates/loom-templates/templates/partial/findings_walk.md" \
        "crates/loom-workflow/src/review/phase_verdict.rs" \
        "tests/fixtures/review/coverage-reductions.md"
      judge_criterion \
        "Evaluate the delivered production review prompt, not keyword counts. Finite review must make current relevant unchanged sibling contracts and acceptance available, broadening uncertain relevance without demanding an exhaustive standing tree audit. Follow the rubric include graph and evaluate each fixture counterexample: lost billing subjects/rounding obligations must be flagged even when remaining checks pass; a replacement-owner claim is accepted only when the replacement verifier demonstrates equivalent behavior and subjects; missing refund coverage is still a finding; removing README from a code-only dependency set is precision, but removing it from an index-agreement checker loses a required obligation. Scrutinize exemptions, skips, discovery exclusions and narrowed responsibilities. An authorized spec change can justify retired obligations; a passing residual suite cannot. Findings must use existing Criterion/Annotation targets, affected bonds, evidence and blocking/deferred/clarify routes, with Options evidence for genuine decisions and LOOM_CONCERN for a completed walk with findings. Fail if review consults only changed or bead-bonded owners, conflates source context with execution, approves an unproven replacement, treats every dependency shrink as loss, or adds a new signoff gate. Report concrete lost subjects/obligations and the actual delivered instruction or route responsible. The fixtures are semantic probes, not precomputed proof that a reviewer performed the evaluation."
    }

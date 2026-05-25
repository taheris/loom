//! FR12 (verdict-gate production wiring). The pure decision function
//! `phase_verdict::decide()` is the single source of truth for the
//! marker → outcome classification; it must be invoked from BOTH
//! `loom run`'s per-bead exit (`run/production.rs`) AND `loom gate
//! review`'s phase-end (`review/production.rs`). No production site
//! may inline ad-hoc marker classification.

use std::path::Path;

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "phase_verdict_decide_called_from_production — run + review production sites must call `decide(...)`";

const RUN_PROD: &str = "crates/loom-workflow/src/run/production.rs";
const REVIEW_PROD: &str = "crates/loom-workflow/src/review/production.rs";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let mut violations = Vec::new();

    check_decide_site(&root, RUN_PROD, "use crate::review::", &mut violations);
    check_decide_site(
        &root,
        REVIEW_PROD,
        "use super::phase_verdict::",
        &mut violations,
    );

    verdict_from(RULE, violations)
}

fn check_decide_site(
    root: &Path,
    rel_path: &str,
    import_prefix: &str,
    violations: &mut Vec<String>,
) {
    let full = root.join(rel_path);
    let Some(body) = read_to_string(&full) else {
        violations.push(format!("{rel_path}:1 production source not found"));
        return;
    };
    if !import_brings_in_decide(&body, import_prefix) {
        violations.push(format!(
            "{rel_path}:1 missing `{import_prefix}{{… decide …}}` import",
        ));
    }
    let calls = body.lines().any(|line| line.contains("decide("));
    if !calls {
        violations.push(format!(
            "{rel_path}:1 no call to `decide(...)` — production must route markers through the gate function",
        ));
    }
}

/// Scan `body` for a `use` statement whose path starts with `import_prefix`
/// and whose brace list imports `decide`. Handles both single-line
/// (`use foo::{decide};`) and rustfmt's multi-line shape
/// (`use foo::{\n    A, B, decide,\n};`).
fn import_brings_in_decide(body: &str, import_prefix: &str) -> bool {
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(import_prefix) {
            continue;
        }
        let mut accumulated = trimmed.to_string();
        while !accumulated.contains(';') {
            let Some(next) = lines.next() else { break };
            accumulated.push(' ');
            accumulated.push_str(next.trim());
        }
        if accumulated.contains("decide") {
            return true;
        }
    }
    false
}

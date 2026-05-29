//! Anti-act-from-inspection walker asserting the inspection-vs-act
//! partition: `loom gate mint` is the SOLE driver-side bd-mutation
//! chokepoint. Every other gate subcommand (`audit` / `verify` /
//! `review` / `judge` / `rubric` / `check` / `test` / `system` /
//! `verify-marker`) is inspection-only.
//!
//! The mint pipeline lives in `crates/loom-workflow/src/mint/` and is
//! dispatched from `crates/loom/src/main.rs` — the `run_gate_mint` arm
//! that serves both standalone `loom gate mint` and `loom loop`'s
//! per-bead `mint --bead` step. The walker scans production sources
//! (`crates/*/src/**/*.rs`) for `mint_findings` / `mint_finding_with_options`
//! invocations and flags any caller outside those two locations so a
//! future change cannot reintroduce a hidden mint side-effect under an
//! inspection subcommand.

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "audit_makes_no_bd_writes_outside_mint_module — \
                    only `loom gate mint` may invoke \
                    `mint_findings` / `mint_finding_with_options`";

const ALLOWED_DIR: &str = "crates/loom-workflow/src/mint/";
const ALLOWED_FILE: &str = "crates/loom/src/main.rs";

const NEEDLE_FINDINGS: &str = concat!("mint_findings", "(");
const NEEDLE_FINDING_WITH_OPTIONS: &str = concat!("mint_finding_with_options", "(");

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(src_files(&root), input, &root);
    let mut violations = Vec::new();
    let needles: [(&str, &str); 2] = [
        (NEEDLE_FINDINGS, "mint_findings"),
        (NEEDLE_FINDING_WITH_OPTIONS, "mint_finding_with_options"),
    ];
    for path in scope {
        let rel_path = rel(&root, &path);
        if is_allowed_caller(&rel_path) {
            continue;
        }
        // The walk's own source carries the literal needles inside
        // string constants; a self-scan would self-flag every entry.
        if rel_path == "crates/loom-walk/src/walk/audit_makes_no_bd_writes_outside_mint_module.rs" {
            continue;
        }
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        for (idx, line) in body.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            for (needle, label) in &needles {
                if line.contains(*needle) {
                    violations.push(format!(
                        "{}:{} `{label}` — mint calls live in `{ALLOWED_DIR}` or `{ALLOWED_FILE}`",
                        rel_path,
                        idx + 1,
                    ));
                    break;
                }
            }
        }
    }
    verdict_from(RULE, violations)
}

fn is_allowed_caller(rel_path: &str) -> bool {
    rel_path.starts_with(ALLOWED_DIR) || rel_path == ALLOWED_FILE
}

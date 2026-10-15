//! Templates under `crates/loom-templates/templates/` must not restate
//! the `LOOM_CONCERN:` / `LOOM_FINDING:` wire format outside the single
//! source-of-truth partial `partial/findings_walk.md`. Other templates
//! that need to reference these markers `{% include %}` the partial;
//! they never re-author the colon-suffixed forms. Invariant 3 from
//! `specs/gate.md`.
//!
//! Only the colon-suffixed forms (the wire-format markers) are flagged.
//! Bare-prose mentions such as *"the `LOOM_CONCERN` marker"* are
//! unaffected.

use std::path::PathBuf;

use super::util::{narrow_to_loom_files, read_to_string, rel, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "template_wire_format_restatement — `LOOM_CONCERN:` / \
                    `LOOM_FINDING:` may only appear in \
                    `partial/findings_walk.md`";

const TEMPLATE_DIR: &str = "crates/loom-templates/templates";

const CANONICAL_PARTIAL: &str = "partial/findings_walk.md";

const WIRE_TOKENS: &[&str] = &["LOOM_CONCERN:", "LOOM_FINDING:"];

fn rule(input: &WalkInput) -> Verdict {
    let root = workspace_root(input);
    let template_root = root.join(TEMPLATE_DIR);
    let canonical = template_root.join(CANONICAL_PARTIAL);
    let scope: Vec<PathBuf> = super::util::files_recursive(input, &template_root, None)
        .into_iter()
        .filter(|p| p != &canonical)
        .collect();
    let scope = narrow_to_loom_files(scope, input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let Some(body) = read_to_string(input, &path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        for (lineno, line) in body.lines().enumerate() {
            for token in WIRE_TOKENS {
                if line.contains(token) {
                    violations.push(format!(
                        "{}:{} restates `{}` — wire format lives only in `{}`",
                        rel_path,
                        lineno + 1,
                        token,
                        CANONICAL_PARTIAL,
                    ));
                }
            }
        }
    }
    verdict_from(RULE, violations)
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("template_wire_format_restatement");

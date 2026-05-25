//! Snapshot-discipline (loom-templates): the snapshot test file
//! `crates/loom-templates/tests/snapshots.rs` must rely on the
//! workspace clippy test exemptions, not per-file `#![allow(...)]` at
//! the crate root. A crate-root allow opts the whole file out of every
//! denied lint at once, including ones the workspace does not exempt
//! for tests.

use std::path::PathBuf;

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_templates_snapshots_no_crate_root_allow — \
                    tests/snapshots.rs must not carry crate-root `#![allow(...)]`";

const TARGET: &str = "crates/loom-templates/tests/snapshots.rs";

const NEEDLE: &str = concat!("#![", "allow(");

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let target = root.join(TARGET);
    let scope: Vec<PathBuf> = if target.is_file() {
        vec![target.clone()]
    } else {
        Vec::new()
    };
    let scope = narrow_to_loom_files(scope, input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        for (lineno, line) in body.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if line.trim_start().starts_with(NEEDLE) {
                violations.push(format!(
                    "{}:{} crate-root `#![allow(...)]` — rely on workspace clippy test exemptions instead",
                    rel_path,
                    lineno + 1,
                ));
            }
        }
    }
    verdict_from(RULE, violations)
}

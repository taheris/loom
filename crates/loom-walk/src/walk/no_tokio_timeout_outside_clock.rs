//! Determinism: `tokio::time::timeout` only inside `SystemClock`.
//! Production code uses `Clock::timeout(...)` injected via
//! `<C: Clock>` so tests under paused tokio runtime can drive
//! deadlines without real sleeps.

use std::path::Path;

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "no_tokio_timeout_outside_clock — exempt only inside SystemClock impl";

const NEEDLE: &str = concat!("tokio::time::", "timeout");

const ALLOWED: &[&str] = &["crates/loom-driver/src/clock/system.rs"];

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(src_files(&root), input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let rel_path = rel(&root, &path);
        if ALLOWED.iter().any(|a| Path::new(&rel_path) == Path::new(a)) {
            continue;
        }
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        for (lineno, line) in body.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if line.contains(NEEDLE) {
                violations.push(format!(
                    "{}:{} `{}` — exempt only inside SystemClock impl",
                    rel_path,
                    lineno + 1,
                    NEEDLE,
                ));
            }
        }
    }
    verdict_from(RULE, violations)
}

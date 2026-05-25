//! Determinism: `tokio::time::sleep` only inside `SystemClock::sleep`
//! and `MockClock::sleep`. Production code accepts `&dyn Clock`
//! instead so tests can advance time deterministically under a paused
//! tokio runtime.

use std::path::Path;

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "no_tokio_sleep_outside_clock — exempt only inside SystemClock/MockClock impls";

const NEEDLE: &str = concat!("tokio::time::", "sleep");

const ALLOWED: &[&str] = &[
    "crates/loom-driver/src/clock/system.rs",
    "crates/loom-driver/src/clock/mock.rs",
];

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
                    "{}:{} `{}` — exempt only inside SystemClock/MockClock impls",
                    rel_path,
                    lineno + 1,
                    NEEDLE,
                ));
            }
        }
    }
    verdict_from(RULE, violations)
}

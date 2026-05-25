//! Determinism: `Instant::now()` and `SystemTime::now()` only inside
//! `SystemClock` and `MockClock`. Production code reads time via
//! `clock.now()` (`&dyn Clock`). `loom-render` carries its own minimal
//! clock impl to keep the renderer free of the tokio dep the driver's
//! full Clock trait pulls in.

use std::path::Path;

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "no_real_clock_outside_system_clock — read time via clock.now()";

const NEEDLE_INSTANT: &str = concat!("Instant::", "now(");
const NEEDLE_SYSTEMTIME: &str = concat!("SystemTime::", "now(");

const ALLOWED: &[&str] = &[
    "crates/loom-driver/src/clock/system.rs",
    "crates/loom-driver/src/clock/mock.rs",
    "crates/loom-render/src/clock.rs",
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
            if line.contains(NEEDLE_INSTANT) || line.contains(NEEDLE_SYSTEMTIME) {
                violations.push(format!(
                    "{}:{} `{}` / `{}` — read time via `clock.now()` (`&dyn Clock`)",
                    rel_path,
                    lineno + 1,
                    NEEDLE_INSTANT,
                    NEEDLE_SYSTEMTIME,
                ));
            }
        }
    }
    verdict_from(RULE, violations)
}

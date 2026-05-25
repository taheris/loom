//! Determinism: no `std::thread::sleep` in production source. Real
//! sleeps make tests flaky on loaded CI runners; the workspace routes
//! every wait through `Clock::sleep` (`&dyn Clock`).

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "no_thread_sleep — use Clock::sleep injected as &dyn Clock";

const NEEDLE_FQN: &str = concat!("std::thread::", "sleep");
const NEEDLE_BARE: &str = concat!("thread::", "sleep(");

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(src_files(&root), input, &root);
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
            if line.contains(NEEDLE_FQN) || line.contains(NEEDLE_BARE) {
                violations.push(format!(
                    "{}:{} `{}` — replace with `MockClock::sleep` under paused tokio runtime",
                    rel_path,
                    lineno + 1,
                    NEEDLE_FQN,
                ));
            }
        }
    }
    verdict_from(RULE, violations)
}

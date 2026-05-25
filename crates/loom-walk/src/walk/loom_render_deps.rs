//! `loom-render` depends on `loom-events` (the public event contract)
//! but never on `loom-driver` or `loom-workflow`. Keeping the renderer
//! free of the driver runtime lets `loom logs`, SSE bridges, and
//! external log tools reuse it without pulling in rusqlite, gix,
//! tokio, etc.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "loom_render_deps — depends on `loom-events`, never `loom-driver` / `loom-workflow`";

const REQUIRED: &str = "loom-events";
const FORBIDDEN: &[&str] = &["loom-driver", "loom-workflow"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let manifest = root.join("crates/loom-render/Cargo.toml");
    let mut violations = Vec::new();

    let Some(body) = read_to_string(&manifest) else {
        violations.push("crates/loom-render/Cargo.toml:1 manifest not readable".to_string());
        return verdict_from(RULE, violations);
    };

    if !body.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with(REQUIRED)
            && trimmed[REQUIRED.len()..]
                .chars()
                .next()
                .is_some_and(|c| c == ' ' || c == '\t' || c == '=')
    }) {
        violations
            .push("crates/loom-render/Cargo.toml:1 missing required dep `loom-events`".to_string());
    }
    for (lineno, raw) in body.lines().enumerate() {
        let trimmed = raw.trim_start();
        for forbidden in FORBIDDEN {
            if trimmed.starts_with(forbidden)
                && trimmed[forbidden.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c == ' ' || c == '\t' || c == '=')
            {
                violations.push(format!(
                    "crates/loom-render/Cargo.toml:{} forbidden dep `{}` — keep renderer free of driver runtime",
                    lineno + 1,
                    forbidden,
                ));
            }
        }
    }

    verdict_from(RULE, violations)
}

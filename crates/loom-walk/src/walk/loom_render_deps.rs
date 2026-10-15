//! `loom-render` depends on `loom-events` (the public event contract)
//! but never on `loom-driver` or `loom-workflow`. Keeping the renderer
//! free of the driver runtime lets `loom logs`, SSE bridges, and
//! external log tools reuse it without pulling in rusqlite, gix,
//! tokio, etc.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

super::manifest::registration!("loom_render_deps", "loom-render");

const RULE: &str =
    "loom_render_deps — depends on `loom-events`, never `loom-driver` / `loom-workflow`";

const REQUIRED: &str = "loom-events";
const FORBIDDEN: &[&str] = &["loom-driver", "loom-workflow"];

fn rule(input: &WalkInput) -> Verdict {
    let root = workspace_root(input);
    let manifest = root.join(MANIFEST);
    let mut violations = Vec::new();

    let Some(body) = read_to_string(input, &manifest) else {
        violations.push("crates/loom-render/Cargo.toml:1 manifest not readable".to_string());
        return verdict_from(RULE, violations);
    };

    analyze(&body)
}

fn analyze(body: &str) -> Verdict {
    let mut violations = Vec::new();

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

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

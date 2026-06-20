//! `loom-events` is a leaf crate — no internal dependency on
//! `loom-driver`, `loom-render`, `agent`, `loom-workflow`, `templates`,
//! `loom-llm`, `loom-skills`, or `loom-tune`. Frontends and log analyzers
//! must be able to consume the event contract without pulling in the driver
//! runtime (rusqlite, gix, tokio). The walk reads `loom-events/Cargo.toml`
//! and flags any internal-crate key that appears anywhere in the manifest.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "loom_events_is_leaf — `loom-events` must not depend on any other internal `loom-*` crate";

const FORBIDDEN: &[&str] = &[
    "loom-driver",
    "loom-render",
    "loom-agent",
    "loom-workflow",
    "loom-templates",
    "loom-llm",
    "loom-skills",
    "loom-tune",
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let manifest = root.join("crates/loom-events/Cargo.toml");
    let mut violations = Vec::new();

    let Some(body) = read_to_string(&manifest) else {
        violations.push("crates/loom-events/Cargo.toml:1 manifest not readable".to_string());
        return verdict_from(RULE, violations);
    };

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
                    "crates/loom-events/Cargo.toml:{} forbidden internal dep `{}` — `loom-events` is a leaf crate",
                    lineno + 1,
                    forbidden,
                ));
            }
        }
    }

    verdict_from(RULE, violations)
}

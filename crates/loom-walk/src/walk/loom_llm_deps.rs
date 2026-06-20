//! `llm` is a public-contract leaf alongside `loom-events` and
//! `templates`. Its `[dependencies]` table must depend on no
//! internal crate other than `loom-events` — pulling `loom-driver`,
//! `agent`, `loom-workflow`, `loom-skills`, or `loom-tune` would invert
//! the consumer-facing dependency direction the spec preserves.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "loom_llm_deps — `llm` may depend only on `loom-events` among internal `loom-*` crates";

const FORBIDDEN: &[&str] = &[
    "loom-driver",
    "loom-agent",
    "loom-workflow",
    "loom-render",
    "loom-templates",
    "loom-skills",
    "loom-tune",
    "loom-gate",
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let manifest = root.join("crates/loom-llm/Cargo.toml");
    let mut violations = Vec::new();

    let Some(body) = read_to_string(&manifest) else {
        violations.push("crates/loom-llm/Cargo.toml:1 manifest not readable".to_string());
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
                    "crates/loom-llm/Cargo.toml:{} forbidden internal dep `{}` — `llm` depends only on `loom-events`",
                    lineno + 1,
                    forbidden,
                ));
            }
        }
    }

    verdict_from(RULE, violations)
}

//! `loom-tune` is an internal tuning crate. It may depend on
//! `loom-events`, `loom-skill`, and third-party parsing/scoring crates,
//! but not on runtime, agent, or workflow crates.

use super::util::verdict_from;
use super::{Verdict, WalkInput};

super::manifest::registration!("loom_tune_deps", "loom-tune");

const RULE: &str =
    "loom_tune_deps — `loom-tune` depends on `loom-events` and `loom-skill`, not runtime crates";

const REQUIRED: &[&str] = &["loom-events", "loom-skill"];

const FORBIDDEN: &[&str] = &[
    "loom-driver",
    "loom-agent",
    "loom-workflow",
    "loom-llm",
    "loom-templates",
    "loom-render",
    "loom-gate",
];

fn analyze(body: &str) -> Verdict {
    let mut violations = Vec::new();

    let keys = match super::manifest::dependency_keys(body, MANIFEST, RULE, &["dependencies"]) {
        Ok(keys) => keys,
        Err(verdict) => return verdict,
    };

    for required in REQUIRED {
        if !keys.contains(*required) {
            violations.push(format!(
                "crates/loom-tune/Cargo.toml:1 missing required dependency `{required}`",
            ));
        }
    }

    for forbidden in FORBIDDEN {
        if keys.contains(*forbidden) {
            violations.push(format!(
                "crates/loom-tune/Cargo.toml:1 forbidden internal dep `{forbidden}` — `loom-tune` may depend only on `loom-events` and `loom-skill` among internal crates",
            ));
        }
    }

    verdict_from(RULE, violations)
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    super::manifest::run(input, MANIFEST, RULE, analyze)
}

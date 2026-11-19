//! `loom-skill` is a public-contract crate for skill artifacts and
//! registry stages. It may depend on `loom-events` for shared newtypes,
//! but not on runtime, agent, template, tune, or workflow crates.

use super::util::verdict_from;
use super::{Verdict, WalkInput};

super::manifest::registration!("loom_skill_deps", "loom-skill");

const RULE: &str =
    "loom_skill_deps — `loom-skill` depends on `loom-events` and not on runtime crates";

const REQUIRED: &[&str] = &["loom-events"];

const FORBIDDEN: &[&str] = &[
    "loom-driver",
    "loom-agent",
    "loom-templates",
    "loom-tune",
    "loom-workflow",
    "loom-llm",
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
                "crates/loom-skill/Cargo.toml:1 missing required dependency `{required}`",
            ));
        }
    }

    for forbidden in FORBIDDEN {
        if keys.contains(*forbidden) {
            violations.push(format!(
                "crates/loom-skill/Cargo.toml:1 forbidden internal dep `{forbidden}` — `loom-skill` may depend only on `loom-events` among internal crates",
            ));
        }
    }

    verdict_from(RULE, violations)
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    super::manifest::run(input, MANIFEST, RULE, analyze)
}

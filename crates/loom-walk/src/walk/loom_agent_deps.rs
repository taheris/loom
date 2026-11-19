//! `loom-agent::direct` wraps `loom-llm::Conversation` to satisfy the
//! `Session` trait; the backend setup path also consumes the materialized
//! skill registry. `loom-agent` must therefore depend on `loom-llm`,
//! `loom-events`, and `loom-skill` directly.

use super::util::verdict_from;
use super::{Verdict, WalkInput};

super::manifest::registration!("loom_agent_deps", "loom-agent");

const RULE: &str =
    "loom_agent_deps — `loom-agent` depends on `loom-llm`, `loom-events`, and `loom-skill`";

const REQUIRED: &[&str] = &["loom-llm", "loom-events", "loom-skill"];

fn analyze(body: &str) -> Verdict {
    let mut violations = Vec::new();

    let keys = match super::manifest::dependency_keys(body, MANIFEST, RULE, &["dependencies"]) {
        Ok(keys) => keys,
        Err(verdict) => return verdict,
    };

    for required in REQUIRED {
        if !keys.contains(*required) {
            violations.push(format!(
                "crates/loom-agent/Cargo.toml:1 missing required dependency `{required}` — `loom-agent` must depend on `loom-llm`, `loom-events`, and `loom-skill`",
            ));
        }
    }

    verdict_from(RULE, violations)
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    super::manifest::run(input, MANIFEST, RULE, analyze)
}

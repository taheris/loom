//! RS-2: every third-party dependency is pinned exactly once under
//! `[workspace.dependencies]` at the workspace root; member crates
//! consume them with `foo = { workspace = true }`. The walk checks the
//! pinning side: the section must exist with at least one entry, and
//! the named third-party deps from the loom-harness spec must each be
//! pinned.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "workspace_deps_pinned — every shared dep is pinned once under [workspace.dependencies]";

const REQUIRED_DEPS: &[&str] = &[
    "tokio",
    "serde",
    "serde_json",
    "thiserror",
    "displaydoc",
    "anyhow",
    "tracing",
    "tracing-subscriber",
    "rusqlite",
    "toml",
    "askama",
    "clap",
    "gix",
    "fd-lock",
];

fn rule(input: &WalkInput) -> Verdict {
    let root = workspace_root(input);
    let manifest = root.join("Cargo.toml");
    let mut violations = Vec::new();

    let Some(body) = read_to_string(input, &manifest) else {
        violations.push("Cargo.toml:1 workspace manifest not readable".to_string());
        return verdict_from(RULE, violations);
    };

    let keys = match super::manifest::dependency_keys(
        &body,
        "Cargo.toml",
        RULE,
        &["workspace", "dependencies"],
    ) {
        Ok(keys) => keys,
        Err(verdict) => return verdict,
    };
    for dep in REQUIRED_DEPS {
        if !keys.contains(*dep) {
            violations.push(format!(
                "Cargo.toml:1 `{dep}` not pinned in [workspace.dependencies]",
            ));
        }
    }

    verdict_from(RULE, violations)
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("workspace_deps_pinned");

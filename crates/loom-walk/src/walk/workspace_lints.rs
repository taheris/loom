//! RS-3: workspace owns lint configuration. The root `Cargo.toml` carries
//! `[workspace.lints.rust]` and `[workspace.lints.clippy]`; every member
//! crate declares `[lints] workspace = true` so the workspace policy
//! applies uniformly. The criterion is the inheritance shape — `cargo
//! clippy --workspace` covers the lint *outcome* via the `loom-clippy`
//! flake check.

use std::path::Path;

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "workspace_lints — root declares workspace lint sections; every member uses [lints] workspace = true";

const LIBRARY_CRATES: &[&str] = &[
    "loom-events",
    "loom-driver",
    "loom-render",
    "loom-agent",
    "loom-workflow",
    "loom-templates",
];

const BINARY_CRATE: &str = "loom";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let workspace_manifest = root.join("Cargo.toml");
    let mut violations = Vec::new();

    match read_to_string(&workspace_manifest) {
        Some(body) => {
            for header in ["[workspace.lints.rust]", "[workspace.lints.clippy]"] {
                if !body.contains(header) {
                    violations.push(format!("Cargo.toml:1 {header} section missing"));
                }
            }
        }
        None => violations.push("Cargo.toml:1 workspace manifest not readable".to_string()),
    }

    let crates_root = root.join("crates");
    for name in std::iter::once(&BINARY_CRATE).chain(LIBRARY_CRATES.iter()) {
        check_member_inherits(&crates_root, name, &mut violations);
    }

    verdict_from(RULE, violations)
}

fn check_member_inherits(crates_root: &Path, name: &str, violations: &mut Vec<String>) {
    let manifest = crates_root.join(name).join("Cargo.toml");
    let Some(body) = read_to_string(&manifest) else {
        violations.push(format!("crates/{name}/Cargo.toml:1 manifest not readable"));
        return;
    };
    let mut in_lints = false;
    let mut found = false;
    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_lints = line == "[lints]";
            continue;
        }
        if !in_lints {
            continue;
        }
        if line.starts_with("workspace") {
            let rest = line.trim_start_matches("workspace").trim_start();
            if let Some(after_eq) = rest.strip_prefix('=')
                && after_eq.trim().starts_with("true")
            {
                found = true;
                break;
            }
        }
    }
    if !found {
        violations.push(format!(
            "crates/{name}/Cargo.toml:1 missing `[lints] workspace = true`",
        ));
    }
}

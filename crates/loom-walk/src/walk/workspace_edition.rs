//! RS-1: workspace declares Rust edition 2024 with resolver "3" at the
//! root `Cargo.toml`, and every member crate inherits the edition via
//! `edition.workspace = true`.

use std::path::Path;

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "workspace_edition — root sets edition=\"2024\" + resolver=\"3\"; members inherit edition.workspace";

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
            if !has_pinned_value(&body, "resolver", "3") {
                violations.push(
                    "Cargo.toml:1 workspace resolver is not \"3\" — add `resolver = \"3\"`"
                        .to_string(),
                );
            }
            if !has_pinned_value(&body, "edition", "2024") {
                violations.push(
                    "Cargo.toml:1 [workspace.package].edition is not \"2024\" — add `edition = \"2024\"`"
                        .to_string(),
                );
            }
        }
        None => {
            violations.push("Cargo.toml:1 workspace manifest not readable".to_string());
        }
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
        violations.push(format!("crates/{name}/Cargo.toml:1 manifest not readable",));
        return;
    };
    if !body
        .lines()
        .any(|line| line.trim_start().starts_with("edition.workspace") && line.contains("true"))
    {
        violations.push(format!(
            "crates/{name}/Cargo.toml:1 missing `edition.workspace = true`",
        ));
    }
}

fn has_pinned_value(body: &str, key: &str, value: &str) -> bool {
    for raw in body.lines() {
        let line = raw.trim_start();
        if !line.starts_with(key) {
            continue;
        }
        let rest = line.trim_start_matches(key).trim_start();
        if !rest.starts_with('=') {
            continue;
        }
        let after_eq = rest.trim_start_matches('=').trim();
        let needle = format!("\"{value}\"");
        if after_eq.starts_with(needle.as_str()) {
            return true;
        }
    }
    false
}

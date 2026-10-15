//! Public-contract crates carry an explicit declaration in their own
//! manifest: `[package.metadata.loom] public_contract = true`. The
//! target v1 public crates are `loom-events`, `loom-protocol`,
//! `loom-llm`, `loom-templates`, and `loom-skill`. The walk confirms
//! every expected crate declares the marker and no other crate does.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "public_contract_crates — exactly loom-events, loom-protocol, loom-llm, loom-templates, loom-skill declare `[package.metadata.loom] public_contract = true`";

const PUBLIC_CRATES: &[&str] = &[
    "loom-events",
    "loom-protocol",
    "loom-llm",
    "loom-templates",
    "loom-skill",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicContractFlag {
    InvalidManifest,
    Missing,
    Value(bool),
}

fn rule(input: &WalkInput) -> Verdict {
    let root = workspace_root(input);
    let mut violations = Vec::new();

    for name in PUBLIC_CRATES {
        let manifest_rel = format!("crates/{name}/Cargo.toml");
        let manifest = root.join(&manifest_rel);
        let Some(body) = read_to_string(input, &manifest) else {
            violations.push(format!("{manifest_rel}:1 manifest not found"));
            continue;
        };
        let flag = public_contract_flag(&body, &manifest_rel, &mut violations);
        match flag {
            PublicContractFlag::Value(true) | PublicContractFlag::InvalidManifest => {}
            PublicContractFlag::Missing | PublicContractFlag::Value(false) => {
                violations.push(format!(
                    "{manifest_rel}:1 missing `[package.metadata.loom] public_contract = true` (found {flag:?})",
                ));
            }
        }
    }

    let crates_dir = root.join("crates");
    for path in super::util::immediate_children(input, &crates_dir) {
        if !super::util::is_dir(input, &path) {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if PUBLIC_CRATES.contains(&name.as_str()) {
            continue;
        }
        let manifest_rel = format!("crates/{name}/Cargo.toml");
        let manifest = path.join("Cargo.toml");
        let Some(body) = read_to_string(input, &manifest) else {
            continue;
        };
        if public_contract_flag(&body, &manifest_rel, &mut violations)
            == PublicContractFlag::Value(true)
        {
            violations.push(format!("{manifest_rel}:1 unexpected `[package.metadata.loom] public_contract = true`; expected set is {}", PUBLIC_CRATES.join(", ")));
        }
    }

    verdict_from(RULE, violations)
}

fn public_contract_flag(
    body: &str,
    manifest_rel: &str,
    violations: &mut Vec<String>,
) -> PublicContractFlag {
    let Ok(value) = toml::from_str::<toml::Value>(body) else {
        violations.push(format!("{manifest_rel}:1 manifest not valid TOML"));
        return PublicContractFlag::InvalidManifest;
    };
    value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("loom"))
        .and_then(|l| l.get("public_contract"))
        .and_then(toml::Value::as_bool)
        .map_or(PublicContractFlag::Missing, PublicContractFlag::Value)
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("public_contract_crates");

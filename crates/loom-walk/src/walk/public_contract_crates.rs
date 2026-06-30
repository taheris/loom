//! Public-contract crates carry an explicit declaration in their own
//! manifest: `[package.metadata.loom] public_contract = true`. The
//! target v1 public crates are `loom-events`, `loom-protocol`,
//! `loom-llm`, `loom-templates`, and `loom-skills`. The walk confirms
//! every expected crate declares the marker and no other crate does.

use std::fs;

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "public_contract_crates — exactly loom-events, loom-protocol, loom-llm, loom-templates, loom-skills declare `[package.metadata.loom] public_contract = true`";

const PUBLIC_CRATES: &[&str] = &[
    "loom-events",
    "loom-protocol",
    "loom-llm",
    "loom-templates",
    "loom-skills",
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let mut violations = Vec::new();

    for name in PUBLIC_CRATES {
        let manifest_rel = format!("crates/{name}/Cargo.toml");
        let manifest = root.join(&manifest_rel);
        let Some(body) = read_to_string(&manifest) else {
            violations.push(format!("{manifest_rel}:1 manifest not found"));
            continue;
        };
        let Some(flag) = public_contract_flag(&body, &manifest_rel, &mut violations) else {
            continue;
        };
        if flag != Some(true) {
            violations.push(format!(
                "{manifest_rel}:1 missing `[package.metadata.loom] public_contract = true` (found {flag:?})",
            ));
        }
    }

    let crates_dir = root.join("crates");
    match fs::read_dir(&crates_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        violations.push(format!("crates:1 failed to read crate entry: {err}"));
                        continue;
                    }
                };
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if PUBLIC_CRATES.contains(&name.as_str()) {
                    continue;
                }
                let manifest_rel = format!("crates/{name}/Cargo.toml");
                let manifest = path.join("Cargo.toml");
                let Some(body) = read_to_string(&manifest) else {
                    continue;
                };
                if public_contract_flag(&body, &manifest_rel, &mut violations) == Some(Some(true)) {
                    violations.push(format!(
                        "{manifest_rel}:1 unexpected `[package.metadata.loom] public_contract = true`; expected set is {}",
                        PUBLIC_CRATES.join(", "),
                    ));
                }
            }
        }
        Err(err) => violations.push(format!("crates:1 failed to enumerate crates: {err}")),
    }

    verdict_from(RULE, violations)
}

fn public_contract_flag(
    body: &str,
    manifest_rel: &str,
    violations: &mut Vec<String>,
) -> Option<Option<bool>> {
    let Ok(value) = toml::from_str::<toml::Value>(body) else {
        violations.push(format!("{manifest_rel}:1 manifest not valid TOML"));
        return None;
    };
    Some(
        value
            .get("package")
            .and_then(|p| p.get("metadata"))
            .and_then(|m| m.get("loom"))
            .and_then(|l| l.get("public_contract"))
            .and_then(|v| v.as_bool()),
    )
}

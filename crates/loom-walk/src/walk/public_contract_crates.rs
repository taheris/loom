//! Public-contract crates carry an explicit declaration in their own
//! manifest: `[package.metadata.loom] public_contract = true`. Three
//! crates currently make that promise — `loom-events`, `llm`,
//! `templates`. The walk reads each crate's `Cargo.toml` and
//! confirms the marker is present and set to `true`.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "public_contract_crates — loom-events, loom-llm, loom-templates declare `[package.metadata.loom] public_contract = true`";

const PUBLIC_CRATES: &[&str] = &["loom-events", "loom-llm", "loom-templates"];

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
        let parsed: Result<toml::Value, _> = body.parse();
        let Ok(value) = parsed else {
            violations.push(format!("{manifest_rel}:1 manifest not valid TOML"));
            continue;
        };
        let flag = value
            .get("package")
            .and_then(|p| p.get("metadata"))
            .and_then(|m| m.get("loom"))
            .and_then(|l| l.get("public_contract"))
            .and_then(|v| v.as_bool());
        if flag != Some(true) {
            violations.push(format!(
                "{manifest_rel}:1 missing `[package.metadata.loom] public_contract = true` (found {flag:?})",
            ));
        }
    }

    verdict_from(RULE, violations)
}

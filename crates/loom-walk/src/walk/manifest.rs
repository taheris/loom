//! Checked manifest analysis shares its rule with the standalone dispatcher.

use std::collections::BTreeSet;
use std::path::PathBuf;

use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Duty, Inventory, Name, Role};
use loom_gate::provider::tracked::Outcome;
use loom_gate::provider::{Definition, Error};

use super::Verdict;

pub(super) fn register<F>(
    snapshot: &Snapshot,
    inventory: &Inventory,
    target: &str,
    checker: &str,
    manifest: &str,
    check: F,
) -> Result<Definition, Error>
where
    F: for<'a> Fn(&mut loom_gate::provider::tracked::Context<'a>) -> Result<Outcome, Error>
        + Send
        + Sync
        + 'static,
{
    let origin = ResourcePath::try_from(PathBuf::from(manifest))?;
    let subjects = inventory
        .subjects()
        .values()
        .filter(|subject| subject.role() == Role::RustSource && subject.origin() == &origin)
        .map(|subject| subject.name().clone())
        .collect();
    Definition::tracked(
        snapshot,
        inventory,
        Name::try_from(target.to_owned())?,
        ResourcePath::try_from(PathBuf::from(checker))?,
        &subjects,
        &BTreeSet::from([Duty::Lint]),
        check,
    )
}

/// Standalone dispatch uses the same analyzer as the checked manifest adapter.
pub(super) fn run(
    input: &super::WalkInput,
    manifest: &str,
    rule: &str,
    analyze: fn(&str) -> Verdict,
) -> Verdict {
    let path = super::util::workspace_root(input).join(manifest);
    let verdict = match super::util::read_to_string(input, &path) {
        Some(body) => analyze(&body),
        None => {
            super::util::verdict_from(rule, vec![format!("{manifest}:1 manifest not readable")])
        }
    };
    input.finish(verdict)
}

/// Parse Cargo dependency tables once instead of maintaining line-based TOML readers.
pub(super) fn dependency_keys(
    body: &str,
    manifest: &str,
    rule: &str,
    section: &[&str],
) -> Result<BTreeSet<String>, Verdict> {
    let fail =
        |detail: String| super::util::verdict_from(rule, vec![format!("{manifest}:1 {detail}")]);
    let parsed: toml::Value =
        toml::from_str(body).map_err(|error| fail(format!("invalid manifest: {error}")))?;
    let mut value = &parsed;
    for key in section {
        value = value
            .get(key)
            .ok_or_else(|| fail(format!("[{}] section missing", section.join("."))))?;
    }
    let table = value
        .as_table()
        .ok_or_else(|| fail(format!("[{}] must be a table", section.join("."))))?;
    Ok(table.keys().cloned().collect())
}

pub(super) fn outcome(verdict: Verdict) -> Outcome {
    if verdict.pass {
        Outcome::Pass
    } else {
        Outcome::Fail {
            detail: verdict.evidence,
        }
    }
}

macro_rules! registration {
    ($target:literal, $package:literal) => {
        pub(super) const NAME: &str = $target;
        const MANIFEST: &str = concat!("crates/", $package, "/Cargo.toml");

        pub(super) fn inputs(
            root: &std::path::Path,
            _input: &$crate::walk::WalkInput,
        ) -> Vec<std::path::PathBuf> {
            vec![root.join(MANIFEST)]
        }

        pub(super) fn register(
            snapshot: &loom_gate::inventory::snapshot::Snapshot,
            inventory: &loom_gate::inventory::Inventory,
        ) -> Result<loom_gate::provider::Definition, loom_gate::provider::Error> {
            $crate::walk::manifest::register(snapshot, inventory, NAME, file!(), MANIFEST, checked)
        }

        fn checked(
            context: &mut loom_gate::provider::tracked::Context<'_>,
        ) -> Result<loom_gate::provider::tracked::Outcome, loom_gate::provider::Error> {
            let path = loom_gate::inventory::snapshot::ResourcePath::try_from(
                std::path::PathBuf::from(MANIFEST),
            )?;
            Ok($crate::walk::manifest::outcome(analyze(
                context.text(&path)?,
            )))
        }
    };
}

pub(super) use registration;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_tables_accept_cargo_table_syntax() {
        for body in [
            "[dependencies]\n'loom-events' = { workspace = true }\n",
            "[dependencies.loom-events]\nworkspace = true\n",
            "dependencies.loom-events.workspace = true\n",
        ] {
            assert_eq!(
                dependency_keys(body, "Cargo.toml", "test", &["dependencies"]).unwrap(),
                BTreeSet::from(["loom-events".to_owned()])
            );
        }
        assert_eq!(
            dependency_keys(
                "[workspace.dependencies]\ntoml = '1'",
                "Cargo.toml",
                "test",
                &["workspace", "dependencies"]
            )
            .unwrap(),
            BTreeSet::from(["toml".to_owned()])
        );
    }

    #[test]
    fn comments_and_dev_dependencies_cannot_supply_runtime_dependencies() {
        let body = "# [dependencies]\n[dev-dependencies]\nloom-events = '1'";
        assert!(dependency_keys(body, "Cargo.toml", "test", &["dependencies"]).is_err());
    }

    #[test]
    fn malformed_manifests_do_not_pass_dependency_checks() {
        for body in ["[dependencies]\nloom-events =", "dependencies = 1"] {
            let verdict =
                dependency_keys(body, "Cargo.toml", "test", &["dependencies"]).unwrap_err();
            assert!(!verdict.pass);
            assert!(verdict.evidence.contains("Cargo.toml:1"));
        }
    }
}

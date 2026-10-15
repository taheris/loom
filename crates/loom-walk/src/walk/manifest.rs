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

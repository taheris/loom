//! A native Nix document check with generated queries and current-context resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

use anyhow::Context as _;
use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Duty, Inventory, Name};
use loom_gate::provider::tracked::Outcome;
use loom_gate::provider::{Definition, Registry};

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() == 1 || args.len() == 2 && args[1] == "--print-inputs",
        "usage: nix-provider <project> [--print-inputs]"
    );
    let snapshot = Snapshot::discover(&PathBuf::from(&args[0]))?;
    let inventory = Inventory::discover(&snapshot)?;
    let flake = ResourcePath::try_from(PathBuf::from("flake.nix"))?;
    let attribute = Name::try_from("heading".to_owned())?;
    let native: Vec<_> = inventory
        .subjects()
        .values()
        .filter(|subject| {
            subject.nix_target().is_some_and(|binding| {
                binding.flake() == &flake && binding.attribute() == &attribute
            })
        })
        .collect();
    anyhow::ensure!(
        native.len() == 1,
        "define one unambiguous native `heading` check before retrying"
    );
    let target = Name::try_from("document-heading".to_owned())?;
    let definition = Definition::hermetic(
        &snapshot,
        &inventory,
        target.clone(),
        native[0].name(),
        &BTreeSet::from([Name::try_from("resource:README.md".to_owned())?]),
        &BTreeSet::from([Duty::Document]),
    )?;
    let registry = Registry::new(&snapshot, vec![definition])?;
    let description = registry.query(std::slice::from_ref(&target))?;
    let mut stdout = std::io::stdout().lock();
    if args.len() == 2 {
        writeln!(stdout, "{description}")?;
        return Ok(());
    }
    let contracts = BTreeMap::from([(
        target,
        ResourcePath::try_from(PathBuf::from("specs/example/tests.md"))?,
    )]);
    let units = registry.resolve_query(&snapshot, &inventory, &description, &contracts)?;
    let observed = units
        .first()
        .context("the requested native check did not resolve")?
        .run()?;
    serde_json::to_writer(&mut stdout, &observed)?;
    writeln!(stdout)?;
    match observed.outcome() {
        Outcome::Pass => Ok(()),
        Outcome::Fail { .. } => {
            anyhow::bail!("document-heading failed; correct the native check or document and retry")
        }
        Outcome::Unavailable { capability, .. } => anyhow::bail!(
            "document-heading requires {capability:?}; provision the declared capability and retry without weakening sandboxing"
        ),
        Outcome::Inapplicable { system } => anyhow::bail!(
            "document-heading is inapplicable on this worker; run on its declared system {}",
            system.as_str()
        ),
    }
}

//! A tracked document check with generated queries and current-context resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

use anyhow::Context as _;
use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Duty, Inventory, Name};
use loom_gate::provider::tracked::{Context, Outcome};
use loom_gate::provider::{Definition, Registry};

fn document_heading(context: &mut Context<'_>) -> Result<Outcome, loom_gate::provider::Error> {
    let subject = Name::try_from("resource:README.md".to_owned())?;
    for path in context.members(&subject)? {
        if !context.text(path)?.starts_with("# ") {
            return Ok(Outcome::Fail {
                detail: "README.md needs a top-level heading; correct the document and rerun"
                    .into(),
            });
        }
    }
    Ok(Outcome::Pass)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let query = args.first().is_some_and(|arg| arg == "--print-inputs");
    let root = std::env::current_dir()?;
    let snapshot = Snapshot::discover(&root)?;
    let inventory = Inventory::discover(&snapshot)?;
    let target = Name::try_from("document-heading".to_owned())?;
    let definition = Definition::tracked(
        &snapshot,
        &inventory,
        target.clone(),
        ResourcePath::try_from(PathBuf::from("src/main.rs"))?,
        &BTreeSet::from([Name::try_from("resource:README.md".to_owned())?]),
        &BTreeSet::from([Duty::Document]),
        document_heading,
    )?;
    let registry = Registry::new(&snapshot, vec![definition])?;
    let targets = if query && args.len() > 1 {
        args[1..]
            .iter()
            .map(|arg| Name::try_from(arg.clone()))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        anyhow::ensure!(
            args.is_empty() || query,
            "usage: provider [--print-inputs [document-heading]]"
        );
        vec![target.clone()]
    };
    let description = registry.query(&targets)?;
    let mut stdout = std::io::stdout().lock();
    if query {
        writeln!(stdout, "{description}")?;
        return Ok(());
    }
    let contracts = BTreeMap::from([(
        target,
        ResourcePath::try_from(PathBuf::from("specs/example/tests.md"))?,
    )]);
    let units = registry.resolve_query(&snapshot, &inventory, &description, &contracts)?;
    let unit = units
        .first()
        .context("the requested provider did not resolve")?;
    let observed = unit.run()?;
    serde_json::to_writer(&mut stdout, &observed)?;
    writeln!(stdout)?;
    anyhow::ensure!(
        matches!(observed.outcome(), Outcome::Pass),
        "document-heading failed; correct README.md and retry"
    );
    Ok(())
}

//! A specifically supported engine observation with explicit configuration and generated queries.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

use anyhow::Context as _;
use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Duty, Inventory, Name};
use loom_gate::provider::effect::{Client, Endpoint};
use loom_gate::provider::tracked::Outcome;
use loom_gate::provider::{Definition, Registry};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    executable: PathBuf,
    endpoint: Endpoint,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.is_empty() || args == ["--print-inputs"],
        "usage: effect-provider [--print-inputs]"
    );
    let snapshot = Snapshot::discover(&std::env::current_dir()?)?;
    let inventory = Inventory::discover(&snapshot)?;
    let configuration = ResourcePath::try_from(PathBuf::from("engine.toml"))?;
    let connection: Connection = toml::from_str(snapshot.text(&configuration)?)?;
    let target = Name::try_from("engine-info".to_owned())?;
    let definition = Definition::container_engine_info(
        &snapshot,
        &inventory,
        target.clone(),
        ResourcePath::try_from(PathBuf::from("src/main.rs"))?,
        &BTreeSet::from([Name::try_from("resource:engine.toml".to_owned())?]),
        &BTreeSet::from([Duty::Test]),
        Client::capture(connection.executable, connection.endpoint)?,
    )?;
    let registry = Registry::new(&snapshot, vec![definition])?;
    let query = registry.query(std::slice::from_ref(&target))?;
    let mut stdout = std::io::stdout().lock();
    if !args.is_empty() {
        writeln!(stdout, "{query}")?;
        return Ok(());
    }
    let contracts = BTreeMap::from([(
        target,
        ResourcePath::try_from(PathBuf::from("specs/example/tests.md"))?,
    )]);
    let units = registry.resolve_query(&snapshot, &inventory, &query, &contracts)?;
    let observed = units
        .first()
        .context("engine-info did not resolve")?
        .run()?;
    serde_json::to_writer(&mut stdout, &observed)?;
    writeln!(stdout)?;
    anyhow::ensure!(
        matches!(observed.outcome(), Outcome::Pass),
        "engine-info did not pass; retain the observed capability/failure distinction and retry after repairing its explicit connection"
    );
    Ok(())
}

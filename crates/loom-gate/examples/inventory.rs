//! Print independently discovered obligations without admitting execution or results.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use loom_gate::inventory::{Inventory, snapshot::Snapshot};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().context("expected the repository root")?);
    if args.next().is_some() {
        anyhow::bail!("expected exactly one repository root");
    }
    let inventory = Inventory::discover(&Snapshot::discover(&root)?)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, inventory.obligations())?;
    writeln!(stdout)?;
    Ok(())
}

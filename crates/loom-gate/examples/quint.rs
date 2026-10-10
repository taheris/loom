//! Acceptance-only dispatcher; this executable cannot authorize publication.

#[path = "../tests/quint/bridge.rs"]
mod bridge;
#[path = "../tests/quint/campaign.rs"]
mod campaign;

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use campaign::Mode;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().context("missing campaign")?;
    let budget_only = command == "budget";
    let selection = if budget_only {
        args.next().context("missing budget campaign")?
    } else {
        command
    };
    let mode = match selection.as_str() {
        "routine" => Mode::Routine,
        "deeper" => Mode::Deeper,
        "bridge-replay" => Mode::Replay,
        _ => bail!("unknown or unimplemented Quint campaign"),
    };
    if budget_only {
        if args.next().is_some() {
            bail!("unexpected budget argument");
        }
        writeln!(
            std::io::stdout().lock(),
            "{}",
            campaign::Plan::shipped(mode)?.total_seconds()?
        )?;
        return Ok(());
    }
    let root = PathBuf::from(args.next().context("missing acceptance source root")?);
    if args.next().is_some() {
        bail!("unexpected Quint campaign argument");
    }
    let receipt = campaign::run(&root, mode, bridge::Adapter::default)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &receipt)?;
    writeln!(stdout)?;
    Ok(())
}

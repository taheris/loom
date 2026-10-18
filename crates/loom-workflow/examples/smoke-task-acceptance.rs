//! Fixed smoke-fixture task binding and pre-container acceptance validation.

use std::io::Write as _;

use anyhow::{Context as _, bail, ensure};
use loom_driver::bd::{BdClient, Bead};
use loom_protocol::acceptance::{Candidates, METADATA_KEY, References};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let workspace = std::env::current_dir()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        let snapshot = loom_gate::acceptance::load(&workspace)?;
        let obligations: Vec<_> = snapshot.obligations().collect();
        ensure!(
            obligations.len() == 1 && obligations[0].reference().spec_label().as_str() == "smoke",
            "the fixed smoke fixture requires one indexed smoke criterion"
        );
        let criteria = References::new(vec![obligations[0].reference().clone()])?;
        snapshot.resolve(&criteria)?;
        let metadata = serde_json::json!({
            METADATA_KEY: serde_json::to_string(&Candidates::Implementation { criteria })?
        });
        writeln!(std::io::stdout(), "{metadata}")?;
    } else if args == ["--validate-task"] {
        let tasks: Vec<Bead> = serde_json::from_reader(std::io::stdin())?;
        ensure!(tasks.len() == 1, "expected exactly one native smoke task");
        let task = &tasks[0];
        let metadata = task
            .metadata
            .get(METADATA_KEY)
            .context("smoke task lacks acceptance")?;
        ensure!(
            matches!(
                Candidates::from_metadata(metadata)?,
                Candidates::Implementation { .. }
            ),
            "smoke task requires implementation acceptance"
        );
        loom_workflow::r#loop::acceptance::resolve(
            &BdClient::<loom_driver::bd::TokioRunner>::new(),
            task,
            &workspace,
        )
        .await?;
    } else {
        bail!("this fixed fixture accepts only --validate-task or no arguments");
    }
    Ok(())
}

//! Container isolation fixture for the existing test-quint acceptance scenario.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use loom_driver::agent::AgentBackend;
use loom_driver::config::LoomTopConfig;
use loom_driver::profile_manifest::ProfileImageManifest;

#[path = "../tests/workspace_safety_history/mod.rs"]
mod scenario;
use scenario::Fixture;

#[tokio::main]
async fn main() -> Result<()> {
    container_scenario().await?;
    serde_json::to_writer(
        std::io::stdout(),
        &serde_json::json!({
            "scenario": "workspace-safety-history", "protected_surfaces": 5,
            "denied_mutations": 20, "feedback_runs": 2,
            "assurance": "workspace isolation only; safety-fact admission remains Evidence-owned"
        }),
    )?;
    writeln!(std::io::stdout())?;
    Ok(())
}

async fn container_scenario() -> Result<()> {
    let manifest = ProfileImageManifest::from_path(&PathBuf::from(
        std::env::var_os("LOOM_PROFILES_MANIFEST")
            .context("missing provisioned worker profile manifest")?,
    ))?;
    let fixture = Fixture::new().await?;
    let loom = LoomTopConfig {
        sccache_dir: Some(fixture.root.path().join("cache")),
        ..LoomTopConfig::default()
    };
    let mut worker = fixture.prepare(&manifest, &loom).await?;
    fs::create_dir_all(fixture.worker_path.join("bin"))?;
    fs::write(
        fixture.worker_path.join("mock-pi.sh"),
        include_str!("../../../tests/mock-pi/pi.sh"),
    )?;
    fs::write(
        fixture.worker_path.join("probe.sh"),
        include_str!("../../../tests/quint/workspace-safety-probe.sh"),
    )?;
    let targets = [
        loom_driver::state::path(fixture.association.operator()),
        fixture.association.operator().join(".loom/state.db.lock"),
        fixture.association.operator().join(".git/config"),
        fixture.association.integration().join(".git/config"),
        fixture.association.integration().join(".loom/state.db"),
    ];
    fs::write(
        fixture.worker_path.join("targets.json"),
        serde_json::to_vec(&targets)?,
    )?;
    fs::write(
        fixture.worker_path.join(".git/loom-workspace.json"),
        b"forged legacy binding",
    )?;
    let pi = fixture.worker_path.join("bin/pi");
    fs::write(
        &pi,
        "#!/usr/bin/env bash\nset -euo pipefail\nbash /workspace/probe.sh\nexec bash /workspace/mock-pi.sh happy-path\n",
    )?;
    fs::set_permissions(pi, fs::Permissions::from_mode(0o755))?;
    worker.spawn.handshake_timeout = Some(std::time::Duration::from_secs(120));
    let mut session = loom_agent::PiBackend::spawn(&worker.spawn)
        .await?
        .prompt("isolation probe")
        .await?;
    let mut completed = false;
    while let Some(event) = session.next_event().await? {
        if matches!(event, loom_events::ParsedAgentEvent::SessionComplete { .. }) {
            completed = true;
        }
    }
    ensure!(
        completed,
        "worker probe did not complete its agent protocol"
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.worker_path.join("isolation-feedback"))?)?;
    ensure!(
        receipt
            == serde_json::json!({"protected_surfaces":5,"denied_mutations":20,"feedback_runs":2}),
        "missing or incomplete container isolation/feedback receipt"
    );
    fixture.check_host_surfaces()?;
    Ok(())
}

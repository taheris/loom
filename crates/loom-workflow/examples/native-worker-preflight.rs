//! Fixed isolated regression for real worker preparation and Wrix stdio startup.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, bail};
use loom_agent::DirectBackend;
use loom_driver::agent::AgentRuntime;
use loom_driver::bd::Bead;
use loom_driver::config::{LoomTopConfig, SkillsConfig};
use loom_driver::git::{GitClient, init_test_repo_with_integration};
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_workflow::agent::run_agent_classified;
use loom_workflow::r#loop::worker::{Preparation, Request, prepare};
use loom_workflow::r#loop::{SessionResult, VERIFICATION_CAPABILITY_UNAVAILABLE_CAUSE};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::args_os().len() == 1,
        "this fixed fixture accepts no arguments"
    );
    let manifest = option_env!("LOOM_TEST_NATIVE_MANIFEST")
        .context("the fixed Nix fixture did not provision its native worker manifest")?;
    let manifest = ProfileImageManifest::from_path(Path::new(manifest))?;
    let fixture = tempfile::tempdir()?;
    let git = init_test_repo_with_integration(fixture.path())?;
    let id = BeadId::new("lm-nprobe")?;
    let label = SpecLabel::new("harness")?;
    let workspace = git.create_worktree(&label, &id).await?.path;
    std::fs::write(
        workspace.join("Cargo.toml"),
        "broken source awaiting agent repair\n",
    )?;
    let source = GitClient::open(&workspace)?;
    source
        .commit_all_allow_empty("Prepare tracked native repair fixture")
        .await?;
    let base = source.head_commit_sha().await?;
    let bead: Bead = serde_json::from_value(serde_json::json!({
        "id":id, "title":"native preflight fixture", "status":"open"
    }))?;
    let profile = ProfileName::new("rust")?;
    let prepared = prepare(Request {
        bead: &bead,
        workspace: &workspace,
        loom_workspace: fixture.path(),
        manifest: &manifest,
        cli_profile: None,
        phase_default: &profile,
        runtime: AgentRuntime::Direct,
        label: &label,
        style_rules: "docs/style-rules.md",
        loom: &LoomTopConfig::default(),
        skills: &SkillsConfig::default(),
        launcher_env: vec![],
        previous_failure: None,
        bead_base: base,
        workspace_recovery: None,
        attempt: 1,
    })
    .await?;
    let Preparation::Ready(worker) = prepared else {
        bail!("real native manifest rejected during worker preparation");
    };
    let result = run_agent_classified::<DirectBackend>(&worker.spawn, None, None, None, None).await;
    match result {
        SessionResult::StaticInfra { cause, error }
            if cause == VERIFICATION_CAPABILITY_UNAVAILABLE_CAUSE =>
        {
            anyhow::ensure!(
                error.contains("78"),
                "native startup lost its exit status: {error}"
            );
            writeln!(
                std::io::stdout(),
                "native-preflight-production-spawn-capability-rejection-ok"
            )?;
            Ok(())
        }
        other => bail!("real isolated native capability failure was misclassified: {other:?}"),
    }
}

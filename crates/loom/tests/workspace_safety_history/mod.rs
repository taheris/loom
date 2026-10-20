use std::fs;
use std::path::PathBuf;

use anyhow::{Result, ensure};
use loom_driver::agent::AgentRuntime;
use loom_driver::bd::Bead;
use loom_driver::config::{LoomTopConfig, SkillsConfig};
use loom_driver::git::{GitClient, commit_all_in, init_test_repo_with_integration};
use loom_driver::identifier::{ProfileName, SpecLabel};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::state::{CacheDb, RecordKind};
use loom_driver::workspace::Association;
use loom_workflow::r#loop::worker::{self, Preparation, Worker};

pub struct Fixture {
    pub root: tempfile::TempDir,
    pub git: GitClient,
    pub association: Association,
    pub worker_path: PathBuf,
    pub bead: Bead,
    pub label: SpecLabel,
}

impl Fixture {
    pub async fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let operator = root.path().join("operator");
        let git = init_test_repo_with_integration(&operator)?;
        let association = Association::resolve(&operator)?;
        let mut bead: Bead = serde_json::from_value(serde_json::json!({
            "id":"lm-safety", "title":"safety isolation", "description":"worker feedback", "status":"open"
        }))?;
        let label = SpecLabel::new("isolation")?;
        let worker_path = git.create_worktree(&label, &bead.id).await?.path;
        fs::create_dir_all(worker_path.join("specs"))?;
        fs::write(
            worker_path.join("specs/isolation.md"),
            "# Isolation\n\n## Success Criteria\n\n- Deterministic worker feedback. [check](test -f feedback-ok)\n",
        )?;
        fs::write(worker_path.join("feedback-ok"), "available\n")?;
        fs::create_dir_all(worker_path.join("docs"))?;
        fs::write(
            worker_path.join("docs/README.md"),
            "- [isolation](../specs/isolation.md)\n",
        )?;
        let snapshot = loom_gate::acceptance::load(&worker_path)?;
        let criteria = loom_protocol::acceptance::References::new(
            snapshot
                .obligations()
                .map(|obligation| obligation.reference().clone())
                .collect(),
        )?;
        bead.metadata.insert(
            loom_protocol::acceptance::METADATA_KEY.to_owned(),
            serde_json::Value::String(serde_json::to_string(
                &loom_protocol::acceptance::Candidates::Implementation { criteria },
            )?),
        );
        commit_all_in(&worker_path, "Add deterministic feedback fixture")?;
        CacheDb::open(loom_driver::state::path(association.operator()))?.put_record(
            RecordKind::Tune,
            "isolation-fixture",
            &"host fixture, not admitted evidence",
        )?;
        Ok(Self {
            root,
            git,
            association,
            worker_path,
            bead,
            label,
        })
    }

    pub async fn prepare(
        &self,
        manifest: &ProfileImageManifest,
        loom: &LoomTopConfig,
    ) -> Result<Worker> {
        let preparation =
            worker::prepare_workspace(&self.git, &self.bead, &self.worker_path).await?;
        match worker::prepare(worker::Request {
            bead: &self.bead,
            workspace: &self.worker_path,
            loom_workspace: self.association.operator(),
            manifest,
            cli_profile: None,
            phase_default: &ProfileName::base(),
            runtime: AgentRuntime::Pi,
            label: &self.label,
            style_rules: "docs/style-rules.md",
            loom,
            skills: &SkillsConfig::default(),
            launcher_env: vec![],
            previous_failure: None,
            bead_base: preparation.bead_base,
            workspace_recovery: preparation.recovery,
            attempt: 0,
        })
        .await?
        {
            Preparation::Ready(worker) => Ok(*worker),
            Preparation::Rejected(_) => anyhow::bail!("valid fixture rejected"),
        }
    }

    pub fn check_host_surfaces(&self) -> Result<()> {
        self.association.recheck()?;
        let integration = Association::resolve(self.association.integration())?;
        ensure!(
            loom_driver::state::path(integration.integration())
                == loom_driver::state::path(self.association.operator())
        );
        ensure!(
            CacheDb::open(loom_driver::state::path(integration.integration()))?
                .record::<String>(RecordKind::Tune, "isolation-fixture")?
                .as_deref()
                == Some("host fixture, not admitted evidence")
        );
        ensure!(Association::resolve(&self.worker_path).is_err());
        Ok(())
    }
}

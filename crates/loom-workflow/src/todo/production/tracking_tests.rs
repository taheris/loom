//! One controlled-agent assembly scenario runs unchanged against both actual adapters.

use super::*;
use anyhow::{Result, anyhow};
use loom_driver::agent::{ProtocolError, WorkerTrackingAccess};
use loom_issues::{
    memory::Backend,
    task::{Create, Kind, Query},
};
use loom_protocol::acceptance::Candidates;

fn protocol(error: impl std::error::Error + Send + Sync + 'static) -> ProtocolError {
    ProtocolError::Io(std::io::Error::other(error))
}

fn prompt_field(prompt: &str, name: &str) -> Result<String, ProtocolError> {
    let prefix = format!("- **{name}**: ");
    prompt
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::to_owned)
        .ok_or_else(|| {
            protocol(std::io::Error::other(format!(
                "missing injected field {name}"
            )))
        })
}

fn fixture_git(workspace: &Path) -> Result<GitClient> {
    let git = loom_driver::git::init_test_repo_with_integration(workspace)?;
    loom_test_support::acceptance::seed_repository_for(workspace, "tracking")?;
    Ok(git)
}

async fn controlled_finalization<T: crate::tracking::Adapter>(
    tracking: T,
    workspace: &Path,
    git: GitClient,
) -> Result<()> {
    let tracking = Arc::new(tracking);
    let state = Arc::new(CacheDb::open(workspace.join(".loom/state.db"))?);
    state.notes_add(
        &SpecLabel::new("tracking")?,
        "implementation",
        "retain operator context",
        1,
    )?;
    let path = workspace.join("tracking-images.json");
    loom_test_support::profile_manifest::write(
        &path,
        r#"{"base":{"pi":{"ref":"fixture","source":"/fixture/image","source_kind":"nix-descriptor"}}}"#,
        Path::new("/fixture/wrix"),
    )?;
    let manifest = Arc::new(ProfileImageManifest::from_path(&path)?);
    let references = References::new(
        loom_gate::acceptance::load(workspace)?
            .obligations()
            .map(|obligation| obligation.reference().clone())
            .collect(),
    )?;
    let mut controller = ProductionTodoController::for_workspace(
        workspace.to_path_buf(),
        Arc::clone(&state),
        manifest,
        ProfileName::base(),
        Arc::new(git),
        Arc::clone(&tracking),
    );
    let agent = Arc::clone(&tracking);
    let task_slot = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&task_slot);
    let summary = crate::todo::run(&mut controller, move |config| async move {
        assert_eq!(config.tracking_access, agent.worker_access());
        assert_eq!(config.to_wrix_json().is_ok(), config.tracking_access == WorkerTrackingAccess::LauncherManaged);
        let head = prompt_field(&config.initial_prompt, "Todo head")?;
        let fingerprint = prompt_field(&config.initial_prompt, "Todo fingerprint")?;
        let parent = BeadId::new(&prompt_field(&config.initial_prompt, "Work epic")?).map_err(protocol)?;
        let task = agent.create(Create {
            title: "controlled agent implementation".into(), parent: Some(parent.clone()),
            notes: Some("original note".into()), metadata: [("operator.note".into(), "unchanged".into())].into(),
            ..Create::default()
        }).await.map_err(protocol)?;
        assert!(!agent.get(&task).await.map_err(protocol)?.metadata.contains_key(METADATA_KEY));
        *captured.lock().map_err(|_| protocol(std::io::Error::other("task capture poisoned")))? = Some(task.clone());
        // The agent returns reference candidates; Rust alone resolves and writes bindings.
        let proposal = serde_json::from_value(serde_json::json!({
            "head":head, "fingerprint":fingerprint, "work_epic":parent, "title":"Controlled implementation",
            "specs":[{"label":"tracking", "outcome":"decomposed", "beads":[task], "assignments":[{"task":task, "criteria":references}]}]
        })).map_err(protocol)?;
        Ok((SessionOutcome { exit_code: 0, cost_usd: None }, None, Some(proposal)))
    }).await?;
    assert_eq!(summary.exit_code, 0);
    assert_eq!(summary.spec_outcomes.len(), 1);
    let task = task_slot
        .lock()
        .map_err(|_| anyhow!("task capture poisoned"))?
        .clone()
        .ok_or_else(|| anyhow!("no controlled task"))?;
    let task = tracking.get(&task).await?;
    assert_eq!(task.metadata["operator.note"], "unchanged");
    assert!(task.notes.as_deref().is_some_and(
        |notes| notes.starts_with("original note") && notes.contains("retain operator context")
    ));
    let Candidates::Implementation { criteria } =
        Candidates::from_metadata(&task.metadata[METADATA_KEY])?
    else {
        return Err(anyhow!("wrong persisted purpose"));
    };
    assert_eq!(
        loom_gate::acceptance::load(workspace)?
            .resolve(&criteria)?
            .obligations()
            .len(),
        1
    );
    assert_eq!(
        state.notes_list(Some(&SpecLabel::new("tracking")?), Some("implementation"))?,
        [] as [loom_driver::state::NoteRow; 0]
    );
    let epics = tracking
        .query(Query {
            kind: Some(Kind::Epic),
            ..Query::default()
        })
        .await?;
    let active = epics
        .iter()
        .filter(|epic| {
            epic.labels
                .iter()
                .any(|label| label.as_str() == "loom:active")
        })
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 1);
    assert_eq!(task.parent.as_ref(), Some(&active[0].id));
    let metadata = epics
        .iter()
        .find(|epic| {
            epic.labels
                .iter()
                .any(|label| label.as_str() == "loom:spec")
        })
        .ok_or_else(|| anyhow!("no spec metadata epic"))?;
    assert!(
        metadata.metadata[TODO_CURSOR_METADATA_KEY]
            .as_str()
            .is_some_and(|head| head.len() == 40)
    );
    assert!(
        !metadata
            .metadata
            .contains_key(super::super::cursor::STATE_KEY)
    );
    Ok(())
}

#[tokio::test]
async fn todo_neutral_tracking_preserves_binding_and_finalization_boundaries() -> Result<()> {
    let lightweight = tempfile::tempdir()?;
    controlled_finalization(
        Backend::default(),
        lightweight.path(),
        fixture_git(lightweight.path())?,
    )
    .await?;
    let native = tempfile::tempdir()?;
    let git = fixture_git(native.path())?;
    let database = tempfile::tempdir()?;
    let tracking = crate::test_support::client(database.path()).await;
    controlled_finalization(tracking, native.path(), git).await
}

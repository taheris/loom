//! Real Todo creation, roster validation, binding and compensation over neutral storage.

use super::*;
use loom_issues::{
    memory::{Backend, Fault, Operation, Timing},
    task::{Create, Kind, Label, Query},
    tracking::{Error, Store},
};
use loom_protocol::{
    acceptance::{Candidates, METADATA_KEY, References},
    todo::{Assignment, NonEmptyVec, TodoSpecOutcome, TodoSuccess},
};

struct Fixture {
    directory: tempfile::TempDir,
    tracking: Backend,
    state: Arc<CacheDb>,
    controller: ProductionTodoController<Backend>,
    success: TodoSuccess,
    task: loom_driver::identifier::BeadId,
    old_active: loom_driver::identifier::BeadId,
    head: String,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let (_, head) = init_workspace(directory.path())?;
        let tracking = Backend::default();
        let old_active = tracking
            .create(Create {
                title: "previous active work".into(),
                kind: Some(Kind::Epic),
                labels: vec![Label::new("loom:active")?],
                ..Create::default()
            })
            .await?;
        let state = Arc::new(CacheDb::open(directory.path().join(".loom/state.db"))?);
        state.notes_add(
            &SpecLabel::new("alpha")?,
            "implementation",
            "retain until finalized",
            1,
        )?;
        let mut controller = ProductionTodoController::for_workspace(
            directory.path().to_path_buf(),
            Arc::clone(&state),
            manifest(directory.path())?,
            ProfileName::new("base")?,
            Arc::new(GitClient::open(directory.path())?),
            Arc::new(tracking.clone()),
        );
        let session = controller.build_session().await?;
        let mut success =
            todo_success(&session.config.initial_prompt, &["alpha", "beta", "gamma"])?;
        let task = tracking
            .create(Create {
                title: "implement pending criterion".into(),
                parent: Some(success.work_epic.clone()),
                notes: Some("original note".into()),
                metadata: [("unrelated".into(), serde_json::json!({"nested": [1, true]}))].into(),
                ..Create::default()
            })
            .await?;
        let snapshot = loom_gate::acceptance::load(directory.path())?;
        let references = References::new(
            snapshot
                .obligations()
                .filter(|obligation| obligation.reference().spec_label().as_str() == "alpha")
                .map(|obligation| obligation.reference().clone())
                .collect(),
        )?;
        let mut rows = success.specs.clone().into_vec();
        let alpha = rows
            .iter_mut()
            .find(|row| row.label.as_str() == "alpha")
            .ok_or_else(|| anyhow!("missing alpha row"))?;
        alpha.outcome = TodoSpecOutcome::Decomposed {
            beads: NonEmptyVec::new(vec![task.clone()])?,
            assignments: NonEmptyVec::new(vec![Assignment {
                task: task.clone(),
                criteria: references,
            }])?,
        };
        success.specs = NonEmptyVec::new(rows)?;
        drop(session);
        Ok(Self {
            directory,
            tracking,
            state,
            controller,
            success,
            task,
            old_active,
            head,
        })
    }

    async fn finalize(&mut self) -> Result<(), TodoError> {
        self.controller
            .record_outcome(
                &SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                },
                None,
                Some(&self.success),
            )
            .await
            .map(|_| ())
    }
}

#[tokio::test]
async fn lightweight_todo_binding_finalization_survives_handle_replacement() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    let proposal = fixture.success.clone();
    let agent_tracking = fixture.tracking.clone();
    let task_id = fixture.task.clone();
    let summary = run_todo_workflow(&mut fixture.controller, move |config| async move {
        assert_eq!(
            config.tracking_access,
            loom_driver::agent::WorkerTrackingAccess::InProcess
        );
        assert!(config.to_wrix_json().is_err());
        let current = agent_tracking
            .get(&task_id)
            .await
            .map_err(|error| loom_driver::agent::ProtocolError::Io(std::io::Error::other(error)))?;
        assert!(
            !current.metadata.contains_key(METADATA_KEY),
            "agent proposes references; only Rust finalization persists acceptance"
        );
        Ok((
            SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            None,
            Some(proposal),
        ))
    })
    .await?;
    assert_eq!(summary.exit_code, 0);
    assert_eq!(summary.spec_outcomes.len(), 3);
    let restarted = fixture.tracking.clone();
    let task = restarted.get(&fixture.task).await?;
    assert_eq!(
        task.metadata["unrelated"],
        serde_json::json!({"nested": [1, true]})
    );
    let Candidates::Implementation { criteria } =
        Candidates::from_metadata(&task.metadata[METADATA_KEY])?
    else {
        return Err(anyhow!("wrong task purpose"));
    };
    let acceptance = loom_gate::acceptance::load(fixture.directory.path())?.resolve(&criteria)?;
    assert_eq!(acceptance.obligations().len(), 1);
    assert!(acceptance.obligations()[0].annotation().pending);
    assert!(task.notes.as_deref().is_some_and(
        |notes| notes.starts_with("original note") && notes.contains("retain until finalized")
    ));
    let work = restarted.get(&fixture.success.work_epic).await?;
    assert!(
        work.labels
            .iter()
            .any(|label| label.as_str() == "loom:active")
    );
    assert!(
        !work
            .labels
            .iter()
            .any(|label| label.as_str() == "loom:todo")
    );
    assert!(
        !restarted
            .get(&fixture.old_active)
            .await?
            .labels
            .iter()
            .any(|label| label.as_str() == "loom:active")
    );
    for epic in restarted
        .query(Query {
            kind: Some(Kind::Epic),
            ..Query::default()
        })
        .await?
    {
        if epic
            .labels
            .iter()
            .any(|label| label.as_str() == "loom:spec")
        {
            assert_eq!(epic.metadata["loom.todo_cursor"], fixture.head);
            assert!(!epic.metadata.contains_key("loom.todo_state"));
        }
    }
    assert_eq!(
        fixture
            .state
            .notes_list(Some(&SpecLabel::new("alpha")?), Some("implementation"))?,
        [] as [loom_driver::state::NoteRow; 0],
    );
    drop(fixture.controller);
    let mut controller = ProductionTodoController::for_workspace(
        fixture.directory.path().to_path_buf(),
        fixture.state,
        manifest(fixture.directory.path())?,
        ProfileName::new("base")?,
        Arc::new(GitClient::open(fixture.directory.path())?),
        Arc::new(restarted),
    );
    assert!(matches!(
        controller.build_session().await,
        Err(TodoError::NoChangedSpecs)
    ));
    Ok(())
}

#[tokio::test]
async fn neutral_todo_compensation_preserves_state_at_every_binding_finalization_write()
-> Result<()> {
    for timing in [Timing::BeforeWrite, Timing::AfterWrite] {
        // Binding, notes, previous active epic, all three spec cursors, and work activation.
        for skip in 0..7 {
            let mut fixture = Fixture::new().await?;
            let mut before = fixture.tracking.query(Query::default()).await?;
            // Labels are an unordered membership set, not a backend ordering guarantee.
            for record in &mut before {
                record
                    .labels
                    .sort_by(|left, right| left.as_str().cmp(right.as_str()));
            }
            fixture.tracking.inject(Fault {
                operation: Operation::Update,
                timing,
                skip,
            })?;
            let outcome = fixture.finalize().await;
            match timing {
                Timing::BeforeWrite => assert!(
                    matches!(outcome, Err(TodoError::Tracking(Error::Transport { .. }))),
                    "write {skip}: {outcome:?}"
                ),
                Timing::AfterWrite => assert!(
                    matches!(outcome, Err(TodoError::Tracking(Error::Uncertain { .. }))),
                    "write {skip}: {outcome:?}"
                ),
            }
            let restarted = fixture.tracking.clone();
            let mut after = restarted.query(Query::default()).await?;
            for record in &mut after {
                record
                    .labels
                    .sort_by(|left, right| left.as_str().cmp(right.as_str()));
            }
            assert_eq!(after, before, "write {skip}, {timing:?}");
            assert_eq!(
                fixture
                    .state
                    .notes_list(Some(&SpecLabel::new("alpha")?), Some("implementation"))?[0]
                    .text,
                "retain until finalized"
            );
            assert!(
                !restarted
                    .get(&fixture.task)
                    .await?
                    .metadata
                    .contains_key(METADATA_KEY)
            );
        }
    }
    Ok(())
}

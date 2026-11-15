//! Storage substitution exercises production policy, not scripted tracker responses.

use loom_issues::{
    memory::{Backend, Fault, Operation, Timing},
    task::{Create, Guard, Id, Kind, Label, Query, Ready, Record, Relation, Status, Update},
    tracking::{Error, Store},
};
use loom_workflow::{
    gate_clarify::{ClarifyApplyOutcome, apply_clarify_or_blocked},
    r#loop::{AgentOutcome, DecisionWaitJournal, LoopError, validate_waiting_outcome},
    resolve::{ResolveError, SpecEpicAction, ensure_spec_metadata_epic, resolve_open_epic},
};

#[tokio::test]
async fn neutral_waiting_policy_reconciles_real_graph_after_handle_replacement() {
    let retained = Backend::default();
    let source = retained
        .create(Create {
            title: "source".into(),
            ..Create::default()
        })
        .await
        .unwrap();
    let blocker = retained
        .create(Create {
            title: "blocker".into(),
            ..Create::default()
        })
        .await
        .unwrap();
    retained
        .relate(&source, &blocker, Relation::Blocks)
        .await
        .unwrap();
    let worker = retained.clone();
    let outcome = validate_waiting_outcome(&worker, &source, AgentOutcome::WaitingRequested)
        .await
        .unwrap();
    assert!(matches!(outcome, AgentOutcome::Waiting { blockers } if blockers.count() == 1));
    drop(worker);
    let restarted = retained.clone();
    restarted.close(&blocker, Some("resolved")).await.unwrap();
    let outcome = validate_waiting_outcome(&restarted, &source, AgentOutcome::WaitingRequested)
        .await
        .unwrap();
    assert!(
        matches!(outcome, AgentOutcome::Failure { error } if error.starts_with("invalid-waiting:"))
    );
}

#[tokio::test]
async fn neutral_metadata_resolution_preserves_duplicate_and_work_scope_boundaries() {
    let retained = Backend::default();
    let label = "tracking".parse().unwrap();
    let planned = ensure_spec_metadata_epic(&retained, &label, true)
        .await
        .unwrap();
    assert_eq!(planned.action, SpecEpicAction::WouldCreate);
    assert_eq!(
        retained.query(Query::default()).await.unwrap(),
        [] as [Record; 0]
    );
    let created = ensure_spec_metadata_epic(&retained, &label, false)
        .await
        .unwrap();
    let SpecEpicAction::Created(metadata) = created.action else {
        panic!("metadata was not created")
    };
    assert_eq!(
        retained.get(&metadata).await.unwrap().status,
        Status::Closed
    );
    let restarted = retained.clone();
    assert_eq!(
        ensure_spec_metadata_epic(&restarted, &label, false)
            .await
            .unwrap()
            .action,
        SpecEpicAction::Existing(metadata)
    );
    assert_eq!(resolve_open_epic(&restarted, &label).await.unwrap(), None);
    let work = restarted
        .create(Create {
            title: "actual work".into(),
            kind: Some(Kind::Epic),
            labels: vec![Label::new("spec:tracking").unwrap()],
            ..Create::default()
        })
        .await
        .unwrap();
    assert_eq!(
        resolve_open_epic(&restarted, &label)
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        work.as_str()
    );
    restarted
        .create(Create {
            title: "duplicate metadata".into(),
            kind: Some(Kind::Epic),
            labels: vec![
                Label::new("loom:spec").unwrap(),
                Label::new("spec:tracking").unwrap(),
            ],
            ..Create::default()
        })
        .await
        .unwrap();
    assert!(matches!(
        ensure_spec_metadata_epic(&restarted, &label, false).await,
        Err(ResolveError::DuplicateSpecEpics { .. })
    ));
    assert_eq!(restarted.query(Query::default()).await.unwrap().len(), 3);
}

async fn claimed_waiter() -> Result<(Backend, Record, Id), Error> {
    let store = Backend::default();
    let source = store
        .create(Create {
            title: "claimed source".into(),
            status: Some(Status::InProgress),
            ..Create::default()
        })
        .await?;
    let blocker = store
        .create(Create {
            title: "blocking prerequisite".into(),
            ..Create::default()
        })
        .await?;
    store.relate(&source, &blocker, Relation::Blocks).await?;
    let source = store.get(&source).await?;
    Ok((store, source, blocker))
}

#[tokio::test]
async fn workflow_tracking_substitution_retains_production_policy() {
    let directory = tempfile::tempdir().unwrap();
    let (retained, source, blocker) = claimed_waiter().await.unwrap();
    let journal = DecisionWaitJournal::new(directory.path());
    let worker = retained.clone();
    let admitted = journal.admit_native_wait(&worker, &source).await.unwrap();
    assert_eq!(admitted.count(), 1);
    assert_eq!(worker.get(&source.id).await.unwrap().status, Status::Open);
    assert!(
        !worker
            .ready(Ready::default())
            .await
            .unwrap()
            .iter()
            .any(|row| row.id == source.id)
    );
    drop(worker);
    drop(journal);
    let restarted = retained.clone();
    restarted
        .close(&blocker, Some("prerequisite completed"))
        .await
        .unwrap();
    assert!(
        restarted
            .ready(Ready::default())
            .await
            .unwrap()
            .iter()
            .any(|row| row.id == source.id)
    );
    let journal = DecisionWaitJournal::new(directory.path());
    assert!(matches!(
        journal.admit_native_wait(&restarted, &source).await,
        Err(LoopError::DecisionWait(_))
    ));
    assert_eq!(
        restarted.get(&source.id).await.unwrap().status,
        Status::Open
    );
}

#[tokio::test]
async fn uncertain_wait_projection_is_reconciled_after_handle_replacement() {
    for timing in [Timing::BeforeWrite, Timing::AfterWrite] {
        let directory = tempfile::tempdir().unwrap();
        let (retained, source, _) = claimed_waiter().await.unwrap();
        retained
            .inject(Fault {
                operation: Operation::Update,
                timing,
                skip: 0,
            })
            .unwrap();
        let worker = retained.clone();
        let result = DecisionWaitJournal::new(directory.path())
            .admit_native_wait(&worker, &source)
            .await;
        match timing {
            Timing::BeforeWrite => assert!(matches!(
                result,
                Err(LoopError::Tracking(Error::Transport { .. }))
            )),
            Timing::AfterWrite => assert!(matches!(
                result,
                Err(LoopError::Tracking(Error::Uncertain { .. }))
            )),
        }
        assert_eq!(
            retained.get(&source.id).await.unwrap().status,
            match timing {
                Timing::BeforeWrite => Status::InProgress,
                Timing::AfterWrite => Status::Open,
            }
        );
        drop(worker);
        let restarted = retained.clone();
        assert_eq!(
            DecisionWaitJournal::new(directory.path())
                .admit_native_wait(&restarted, &source)
                .await
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            restarted.get(&source.id).await.unwrap().status,
            Status::Open
        );
    }
}

#[tokio::test]
async fn lightweight_clarification_preserves_brief_and_independent_holds() {
    let (store, source, _) = claimed_waiter().await.unwrap();
    store
        .update(
            &source.id,
            Update {
                add_labels: vec![Label::new("loom:infra").unwrap()],
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    assert!(matches!(
        DecisionWaitJournal::new(tempfile::tempdir().unwrap().path())
            .admit_native_wait(&store, &source)
            .await,
        Err(LoopError::DecisionWait(_))
    ));
    assert_eq!(
        store.get(&source.id).await.unwrap().status,
        Status::InProgress
    );
    let notes = "## Options — choose a path\n\n### Option 1 — Keep\nCost: maintenance.\n\n### Option 2 — Change\nCost: churn.\n";
    store
        .update(
            &source.id,
            Update {
                notes: Some(notes.into()),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    assert_eq!(
        apply_clarify_or_blocked(&store, &source.id).await.unwrap(),
        ClarifyApplyOutcome::Clarify
    );
    let current = store.get(&source.id).await.unwrap();
    assert_eq!(current.notes.as_deref(), Some(notes));
    assert_eq!(current.status, Status::Blocked);
    assert!(current.labels.contains(&Label::new("loom:infra").unwrap()));
    assert!(
        current
            .labels
            .contains(&Label::new("loom:clarify").unwrap())
    );
}

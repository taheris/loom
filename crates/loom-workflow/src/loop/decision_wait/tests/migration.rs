use super::*;
mod human_hold;
mod recovery;
use loom_driver::bd::{BdError, CommandRunner, RunOutput};
use std::ffi::OsString;
use std::time::Duration;

#[cfg(test)]
#[tokio::test]
async fn work_root_candidates_wait_on_native_prerequisites_and_resume_open() {
    let f = Fixture::new().await;
    for id in &f.decisions {
        f.bd.reparent(id, &f.root).await.unwrap();
        f.bd.dep_add(&f.source.id, id).await.unwrap();
        f.bd.update(
            id,
            UpdateOpts {
                status: Some(Status::Blocked),
                set_metadata: vec![("loom.decision_candidate".into(), serde_json::json!({
                "version":1,"producer":f.source.id,"root":f.root,"affected_work":[f.source.id]
            }).to_string())],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
    }
    let outcome = f.admit().await;
    assert!(
        matches!(outcome, AgentOutcome::Waiting { blockers: WaitPrerequisites::Native(ref blockers) } if blockers.ids().count() == 2),
        "{outcome:?}"
    );
    assert_eq!(
        f.bd.show(&f.source.id).await.unwrap().status,
        Status::Blocked
    );
    assert!(
        f.bd.dep_add(&f.root, &f.decisions[0]).await.is_err(),
        "native descendant guard"
    );
    assert!(
        f.bd.reparent(&f.root, &f.decisions[0]).await.is_err(),
        "native ancestor ownership guard"
    );
    assert!(
        f.bd.dep_add(&f.decisions[0], &f.source.id).await.is_err(),
        "native scheduling cycle guard"
    );
    assert_eq!(
        f.journal()
            .load(&f.source.id)
            .unwrap()
            .unwrap()
            .record
            .decisions
            .len(),
        2
    );
    assert!(
        !f.bd
            .ready(ReadyOpts {
                parent: Some(f.root.clone()),
                ..ReadyOpts::default()
            })
            .await
            .unwrap()
            .iter()
            .any(|work| work.id == f.source.id)
    );
    f.bd.close(&f.decisions[0], Some("human chose Keep"))
        .await
        .unwrap();
    assert!(held(&f.bd, &f.journal(), &f.source).await.unwrap());
    f.bd.close(&f.decisions[1], Some("human chose Change"))
        .await
        .unwrap();
    assert!(!held(&f.bd, &f.journal(), &f.source).await.unwrap());
    let source = f.bd.show(&f.source.id).await.unwrap();
    assert_eq!(
        source.status,
        Status::Open,
        "answering does not meet implementation acceptance"
    );
    assert!(!source.metadata.contains_key("loom.dependency_wait"));
    assert!(
        f.bd.ready(ReadyOpts {
            parent: Some(f.root.clone()),
            ..ReadyOpts::default()
        })
        .await
        .unwrap()
        .iter()
        .any(|work| work.id == f.source.id)
    );
}

struct InterruptReparent(IsolatedBeads);
impl CommandRunner for InterruptReparent {
    async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
        if args.first().is_some_and(|arg| arg == "update")
            && args.iter().any(|arg| arg == "--parent")
        {
            return Ok(RunOutput {
                status: 1,
                stdout: Vec::new(),
                stderr: b"injected interruption before reparent".to_vec(),
            });
        }
        self.0.run(args, timeout).await
    }
}

#[cfg(test)]
#[tokio::test]
async fn interrupted_native_migration_recovers_without_losing_the_batch() {
    let f = Fixture::new().await;
    let failing = BdClient::with_runner(InterruptReparent(IsolatedBeads::attach(f.dir.path())));
    assert!(
        reconcile_session(
            &failing,
            &f.journal(),
            &f.source,
            Fixture::session(&f.decisions),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.bd.show(&f.decisions[0]).await.unwrap().parent.as_ref(),
        Some(&f.source.id)
    );
    let journal = DecisionWaitJournal::new(f.dir.path());
    journal
        .recover_inventory(&f.bd, Some(&f.root), &SpecLabel::new("loop").unwrap())
        .await
        .unwrap();
    for id in &f.decisions {
        let decision = f.bd.show(id).await.unwrap();
        assert_eq!(decision.parent.as_ref(), Some(&f.root));
        let provenance: serde_json::Value = serde_json::from_str(
            decision.metadata["loom.decision_candidate"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(provenance["producer"], f.source.id.as_str());
        assert_eq!(provenance["root"], f.root.as_str());
    }
    assert_eq!(f.bd.show(&f.decisions[0]).await.unwrap().description, BRIEF);
    assert_eq!(
        f.bd.show(&f.source.id).await.unwrap().status,
        Status::Blocked
    );
    assert_eq!(
        f.bd.dependency_snapshot(&f.source.id)
            .await
            .unwrap()
            .active_blockers()
            .len(),
        2
    );
}

#[cfg(test)]
#[tokio::test]
async fn closed_context_from_two_producers_preserves_all_three_native_outcomes() {
    let f = Fixture::new().await;
    f.bd.reparent(&f.decisions[1], &f.sibling).await.unwrap();
    let third =
        f.bd.create(CreateOpts {
            title: "second decision from other producer".into(),
            description: BRIEF.into(),
            issue_type: Some(IssueType::Decision),
            parent: Some(f.sibling.clone()),
            ..CreateOpts::default()
        })
        .await
        .unwrap();
    let ids = vec![f.decisions[0].clone(), f.decisions[1].clone(), third];
    let runner = IsolatedBeads::attach(f.dir.path());
    let mut original = Vec::new();
    for id in &ids {
        f.bd.close(id, Some("recorded human outcome: Keep"))
            .await
            .unwrap();
        let output = runner
            .run(
                ["show", id.as_str(), "--json"].map(OsString::from).to_vec(),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert!(output.success());
        let snapshot: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(snapshot[0]["close_reason"], "recorded human outcome: Keep");
        original.push(snapshot);
    }
    let wire = format!(
        "LOOM_CLARIFY: {{\"decisions\":{}}}\nLOOM_NOOP",
        serde_json::to_string(&ids).unwrap()
    );
    let session = || SessionResult::Decoded {
        outcome: SessionOutcome {
            exit_code: 0,
            cost_usd: None,
        },
        output: Arc::new(decode(&wire, OutputPhase::Loop)),
    };
    for _ in 0..2 {
        assert_eq!(
            reconcile_session(&f.bd, &f.journal(), &f.source, session(), None)
                .await
                .unwrap(),
            AgentOutcome::Success
        );
    }
    assert_eq!(
        f.journal()
            .load(&f.source.id)
            .unwrap()
            .unwrap()
            .record
            .decisions
            .len(),
        3
    );
    for (index, id) in ids.iter().enumerate() {
        let decision = f.bd.show(id).await.unwrap();
        assert_eq!(decision.parent.as_ref(), Some(&f.root));
        assert_eq!(decision.status, Status::Closed);
        assert!(
            !decision
                .labels
                .iter()
                .any(loom_driver::bd::Label::is_clarify)
        );
        let output = runner
            .run(
                ["show", id.as_str(), "--json"].map(OsString::from).to_vec(),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        let after: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        for key in ["id", "description", "notes", "close_reason", "closed_at"] {
            assert_eq!(after[0][key], original[index][0][key], "{key}");
        }
        let provenance: serde_json::Value = serde_json::from_str(
            decision.metadata["loom.decision_candidate"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            provenance["producer"],
            if index == 0 {
                f.source.id.as_str()
            } else {
                f.sibling.as_str()
            }
        );
        assert_eq!(provenance["root"], f.root.as_str());
    }
    assert_eq!(f.bd.show(&f.source.id).await.unwrap().status, Status::Open);
}

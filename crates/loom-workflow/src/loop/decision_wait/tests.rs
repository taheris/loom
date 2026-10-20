use super::*;
use crate::r#loop::WaitPrerequisites;
use loom_driver::bd::UpdateOpts;

#[cfg(test)]
mod disposition;
mod graph;
mod inventory;
mod migration;
mod recovery;
mod scope;
use crate::test_support::{IsolatedBeads, client};
use loom_driver::agent::SessionOutcome;
use loom_driver::bd::{CreateOpts, ReadyOpts};
use loom_driver::identifier::SpecLabel;
use loom_protocol::output::{Phase as OutputPhase, decode};
use std::sync::Arc;

const BRIEF: &str = "## Options — choose compatibility\n\n### Option 1 — Keep\nCost: debt.\n\n### Option 2 — Change\nCost: churn.\n";

struct Fixture {
    dir: tempfile::TempDir,
    bd: BdClient<IsolatedBeads>,
    root: BeadId,
    source: Bead,
    sibling: BeadId,
    decisions: Vec<BeadId>,
}

#[cfg(test)]
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bd = client(dir.path()).await;
        let root = bd
            .create(CreateOpts {
                title: "work root".into(),
                issue_type: Some(IssueType::Epic),
                labels: vec!["spec:loop".into()],
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let source_id = bd
            .create(CreateOpts {
                title: "producer".into(),
                parent: Some(root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let sibling = bd
            .create(CreateOpts {
                title: "independent work".into(),
                parent: Some(root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let mut decisions = Vec::new();
        for brief in [BRIEF, "missing Options; original context"] {
            decisions.push(
                bd.create(CreateOpts {
                    title: "human decision".into(),
                    description: brief.into(),
                    issue_type: Some(IssueType::Decision),
                    parent: Some(source_id.clone()),
                    notes: Some("human history to retain".into()),
                    ..CreateOpts::default()
                })
                .await
                .unwrap(),
            );
        }
        let source = bd.show(&source_id).await.unwrap();
        Self {
            dir,
            bd,
            root,
            source,
            sibling,
            decisions,
        }
    }

    fn journal(&self) -> DecisionWaitJournal {
        DecisionWaitJournal::new(self.dir.path())
    }

    fn session(ids: &[BeadId]) -> SessionResult {
        let wire = format!(
            "LOOM_CLARIFY: {{\"decisions\":{}}}\nLOOM_WAITING",
            serde_json::to_string(ids).unwrap()
        );
        SessionResult::Decoded {
            outcome: SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            output: Arc::new(decode(&wire, OutputPhase::Loop)),
        }
    }

    async fn admit(&self) -> AgentOutcome {
        reconcile_session(
            &self.bd,
            &self.journal(),
            &self.source,
            Self::session(&self.decisions),
            None,
        )
        .await
        .unwrap()
    }
}

#[cfg(test)]
#[test]
fn attribution_is_strict_and_nonempty() {
    for payload in [
        r#"{"version":1,"source":"lm-source","root":"lm-root","decisions":[]}"#,
        r#"{"version":1,"source":"lm-other","root":"lm-root","decisions":["lm-decision"]}"#,
        r#"{"version":1,"source":"lm-source","root":"lm-root","decisions":["lm-decision","lm-decision"]}"#,
    ] {
        let source: Bead = serde_json::from_value(serde_json::json!({
            "id":"lm-source","title":"source","status":"open",
            "metadata": {HOLD_KEY: payload}
        }))
        .unwrap();
        assert!(record(&source).is_err());
    }
}

#[cfg(test)]
#[tokio::test]
async fn native_descendant_wait_preserves_guards_and_releases_only_owned_state() {
    let f = Fixture::new().await;
    assert!(f.bd.dep_add(&f.source.id, &f.decisions[0]).await.is_err());
    f.bd.update(
        &f.source.id,
        UpdateOpts {
            set_metadata: vec![("operator.note".into(), "keep".into())],
            notes: Some("source history".into()),
            ..UpdateOpts::default()
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(f.admit().await, AgentOutcome::Waiting { blockers: WaitPrerequisites::Native(ref blockers) } if blockers.ids().count() == 2)
    );
    let source = f.bd.show(&f.source.id).await.unwrap();
    assert_eq!(source.status, Status::Open);
    assert_eq!(source.parent, f.source.parent);
    assert_eq!(
        f.bd.dependency_snapshot(&source.id)
            .await
            .unwrap()
            .active_blockers()
            .into_iter()
            .collect::<HashSet<_>>(),
        f.decisions.iter().cloned().collect::<HashSet<_>>(),
    );
    let ready =
        f.bd.ready(ReadyOpts {
            limit: None,
            parent: Some(f.root.clone()),
            ..ReadyOpts::default()
        })
        .await
        .unwrap();
    assert!(!ready.iter().any(|bead| bead.id == source.id));
    assert!(ready.iter().any(|bead| bead.id == f.sibling));
    let valid = f.bd.show(&f.decisions[0]).await.unwrap();
    let defect = f.bd.show(&f.decisions[1]).await.unwrap();
    assert!(
        valid
            .labels
            .iter()
            .any(|label| label.as_str() == "loom:clarify")
    );
    assert!(
        defect
            .labels
            .iter()
            .any(|label| label.as_str() == "loom:blocked")
    );
    assert_eq!(valid.description, BRIEF);
    assert!(
        defect
            .notes
            .as_ref()
            .unwrap()
            .contains("human history to retain")
    );
    let first = f.journal().load(&source.id).unwrap().unwrap().record;
    assert!(matches!(f.admit().await, AgentOutcome::Waiting { .. }));
    assert_eq!(first, f.journal().load(&source.id).unwrap().unwrap().record);
    let raw_human =
        f.bd.create(CreateOpts {
            title: "unadmitted human decision".into(),
            issue_type: Some(IssueType::Decision),
            parent: Some(f.root.clone()),
            description: BRIEF.into(),
            ..CreateOpts::default()
        })
        .await
        .unwrap();
    assert!(
        f.bd.ready(ReadyOpts {
            limit: None,
            parent: Some(f.root.clone()),
            ..ReadyOpts::default()
        })
        .await
        .unwrap()
        .iter()
        .any(|bead| bead.id == raw_human)
    );
    let batch = super::super::schedule::parallel_ready_batch(
        &f.bd,
        f.dir.path(),
        &SpecLabel::new("loop").unwrap(),
        Some(&f.root),
        8,
        &[],
        &HashSet::new(),
    )
    .await
    .unwrap();
    assert!(batch.iter().any(|bead| bead.id == f.sibling));
    assert!(!batch.iter().any(|bead| bead.id == source.id));
    assert!(!batch.iter().any(|bead| bead.id == raw_human));
    f.bd.dep_add(&source.id, &f.sibling).await.unwrap();
    f.bd.close(&f.decisions[0], Some("human chose Keep"))
        .await
        .unwrap();
    assert!(held(&f.bd, &f.journal(), &source).await.unwrap());
    f.bd.close(&f.decisions[1], Some("human resolved defective brief"))
        .await
        .unwrap();
    assert!(held(&f.bd, &f.journal(), &source).await.unwrap());
    assert!(
        f.bd.dependency_snapshot(&source.id)
            .await
            .unwrap()
            .active_blockers()
            .contains(&f.sibling)
    );
    f.bd.close(&f.sibling, Some("independent prerequisite met"))
        .await
        .unwrap();
    assert!(!held(&f.bd, &f.journal(), &source).await.unwrap());
    let resumed = f.bd.show(&source.id).await.unwrap();
    assert_eq!(resumed.status, Status::Open);
    assert_eq!(resumed.notes.as_deref(), Some("source history"));
    assert_eq!(resumed.metadata["operator.note"], "keep");
    assert!(!resumed.metadata.contains_key(HOLD_KEY));
    assert_eq!(
        f.bd.dependency_snapshot(&source.id)
            .await
            .unwrap()
            .active_blockers(),
        [] as [BeadId; 0]
    );
    assert_eq!(
        f.journal().load(&source.id).unwrap().unwrap().phase,
        Phase::InventoryAdmitted
    );
    assert!(!f.journal().has_pending_for(&f.root).unwrap());
}

#[cfg(test)]
#[tokio::test]
async fn incomplete_inventory_and_forged_native_attribution_have_no_queue_effects() {
    let f = Fixture::new().await;
    let outcome = reconcile_session(
        &f.bd,
        &f.journal(),
        &f.source,
        Fixture::session(&f.decisions[..1]),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(outcome, AgentOutcome::Failure { .. }));
    assert!(f.journal().load(&f.source.id).unwrap().is_none());
    for id in &f.decisions {
        assert_eq!(f.bd.show(id).await.unwrap().status, Status::Open);
    }
    f.bd.update(
        &f.source.id,
        UpdateOpts {
            add_labels: vec![HOLD_LABEL.into()],
            set_metadata: vec![(HOLD_KEY.into(), "{}".into())],
            ..UpdateOpts::default()
        },
    )
    .await
    .unwrap();
    assert!(matches!(f.admit().await, AgentOutcome::Failure { .. }));
    assert!(
        held(&f.bd, &f.journal(), &f.bd.show(&f.source.id).await.unwrap())
            .await
            .is_err()
    );
    assert_eq!(
        f.bd.show(&f.source.id).await.unwrap().metadata[HOLD_KEY],
        "{}"
    );
}

#[cfg(test)]
#[tokio::test]
async fn restart_recovers_intent_and_human_takeover_without_reopening() {
    let f = Fixture::new().await;
    let journal = f.journal();
    journal
        .save(&Entry {
            record: Record {
                version: 1,
                source: f.source.id.clone(),
                root: f.root.clone(),
                decisions: f.decisions.clone(),
            },
            phase: Phase::ProjectionPending,
            context: None,
            native_wait: false,
        })
        .unwrap();
    assert!(held(&f.bd, &f.journal(), &f.source).await.unwrap());
    assert_eq!(
        journal.load(&f.source.id).unwrap().unwrap().phase,
        Phase::InventoryAdmitted
    );
    f.bd.update(
        &f.source.id,
        UpdateOpts {
            status: Some(Status::Deferred),
            notes: Some("human takeover".into()),
            ..UpdateOpts::default()
        },
    )
    .await
    .unwrap();
    for id in &f.decisions {
        f.bd.close(id, Some("human answer")).await.unwrap();
    }
    assert!(held(&f.bd, &f.journal(), &f.source).await.unwrap());
    let source = f.bd.show(&f.source.id).await.unwrap();
    assert_eq!(source.status, Status::Deferred);
    assert_eq!(source.notes.as_deref(), Some("human takeover"));
    assert!(!source.metadata.contains_key("loom.dependency_wait"));
    assert!(!journal.has_pending_for(&f.root).unwrap());
}

#[cfg(test)]
#[tokio::test]
async fn changed_root_or_corrupt_projection_cannot_release_owned_hold() {
    let f = Fixture::new().await;
    assert!(matches!(f.admit().await, AgentOutcome::Waiting { .. }));
    f.bd.update(
        &f.source.id,
        UpdateOpts {
            set_metadata: vec![("loom.dependency_wait".into(), "{}".into())],
            ..UpdateOpts::default()
        },
    )
    .await
    .unwrap();
    assert!(held(&f.bd, &f.journal(), &f.source).await.is_err());
    assert!(!matches!(
        f.journal().blocks_scope(&f.bd, &f.root).await,
        Ok(false)
    ));
    let other =
        f.bd.create(CreateOpts {
            title: "other root".into(),
            issue_type: Some(IssueType::Epic),
            ..CreateOpts::default()
        })
        .await
        .unwrap();
    f.bd.reparent(&f.source.id, &other).await.unwrap();
    assert!(held(&f.bd, &f.journal(), &f.source).await.is_err());
    assert!(matches!(f.admit().await, AgentOutcome::Failure { .. }));
    assert_eq!(f.bd.show(&f.source.id).await.unwrap().status, Status::Open);
}

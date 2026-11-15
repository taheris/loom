use super::MemoryFixture as Fixture;
use super::*;

#[cfg(test)]
#[tokio::test]
async fn restart_finishes_partial_staging_without_rewriting_human_context() {
    let f = Fixture::new().await;
    let entry = Entry {
        record: Record {
            version: 1,
            source: f.source.id.clone(),
            root: f.root.clone(),
            decisions: f.decisions.clone(),
        },
        phase: Phase::Active,
        context: None,
        native_wait: false,
    };
    f.journal().save(&entry).unwrap();
    f.bd.update(
        &f.source.id,
        Update {
            add_labels: vec![Label::new(HOLD_LABEL).unwrap()],
            set_metadata: [(
                HOLD_KEY.into(),
                serde_json::to_string(&entry.record).unwrap().into(),
            )]
            .into(),
            ..Update::default()
        },
        Guard::Status(Status::Open),
    )
    .await
    .unwrap();
    crate::gate_clarify::apply_guarded_decision_report(
        &f.bd,
        f.bd.get(&f.decisions[0]).await.unwrap(),
    )
    .await
    .unwrap();
    let first = f.bd.get(&f.decisions[0]).await.unwrap();
    assert!(held(&f.bd, &f.journal(), &f.source).await.unwrap());
    let migrated = f.bd.get(&f.decisions[0]).await.unwrap();
    assert_eq!(first.description, migrated.description);
    assert_eq!(first.notes, migrated.notes);
    assert_eq!(first.labels, migrated.labels);
    assert_eq!(migrated.parent.as_ref(), Some(&f.root));
    let repaired = f.bd.get(&f.decisions[1]).await.unwrap();
    assert_eq!(repaired.status, Status::Blocked);
    assert!(
        repaired
            .notes
            .as_ref()
            .unwrap()
            .contains("human history to retain")
    );
    assert!(held(&f.bd, &f.journal(), &f.source).await.unwrap());
    assert_eq!(repaired, f.bd.get(&f.decisions[1]).await.unwrap());
}

#[cfg(test)]
#[tokio::test]
async fn guarded_brief_repair_preserves_same_status_human_notes_written_after_snapshot() {
    let f = Fixture::new().await;
    let snapshot = f.bd.get(&f.decisions[1]).await.unwrap();
    let source = f.bd.get(&f.source.id).await.unwrap();
    let sibling = f.bd.get(&f.decisions[0]).await.unwrap();
    let human_notes = "human replaced draft after the driver read it; retain this disposition";
    f.bd.update(
        &snapshot.id,
        Update {
            notes: Some(human_notes.into()),
            ..Update::default()
        },
        Guard::Status(snapshot.status),
    )
    .await
    .unwrap();
    let report = crate::gate_clarify::apply_guarded_decision_report(&f.bd, snapshot)
        .await
        .unwrap();
    assert_eq!(
        report.outcome,
        crate::gate_clarify::ClarifyApplyOutcome::BlockedClarifyWithoutOptions
    );
    let decision = f.bd.get(&f.decisions[1]).await.unwrap();
    let notes = decision.notes.unwrap();
    assert!(
        notes.starts_with(human_notes),
        "stale snapshots cannot replace current human notes"
    );
    assert!(notes.contains("clarify-without-options"));
    assert!(
        !notes.contains("human history to retain"),
        "do not resurrect an intentionally replaced draft"
    );
    assert_eq!(f.bd.get(&f.source.id).await.unwrap(), source);
    assert_eq!(f.bd.get(&f.decisions[0]).await.unwrap(), sibling);
}

#[cfg(test)]
#[tokio::test]
async fn restart_acknowledges_completed_release_without_reopening_work() {
    let f = Fixture::new().await;
    assert!(matches!(f.admit().await, AgentOutcome::Waiting { .. }));
    for id in &f.decisions {
        f.bd.close(id, Some("human answer")).await.unwrap();
    }
    assert!(!held(&f.bd, &f.journal(), &f.source).await.unwrap());
    f.bd.update(
        &f.source.id,
        Update {
            notes: Some("retained source draft".into()),
            set_metadata: [("operator.note".into(), "keep".into())].into(),
            ..Update::default()
        },
        Guard::None,
    )
    .await
    .unwrap();
    let before = f.bd.get(&f.source.id).await.unwrap();
    assert!(!held(&f.bd, &f.journal(), &f.source).await.unwrap());
    assert_eq!(before, f.bd.get(&f.source.id).await.unwrap());
    assert_eq!(before.status, Status::Open);
    assert_eq!(
        f.journal().load(&f.source.id).unwrap().unwrap().phase,
        Phase::InventoryAdmitted
    );
}

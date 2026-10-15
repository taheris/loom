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
    };
    f.journal().save(&entry).unwrap();
    f.bd.update(
        &f.source.id,
        UpdateOpts {
            if_status: Some(Status::Open),
            add_labels: vec![HOLD_LABEL.into()],
            set_metadata: vec![(
                HOLD_KEY.into(),
                serde_json::to_string(&entry.record).unwrap(),
            )],
            ..UpdateOpts::default()
        },
    )
    .await
    .unwrap();
    crate::gate_clarify::apply_guarded_decision_report(
        &f.bd,
        f.bd.show(&f.decisions[0]).await.unwrap(),
    )
    .await
    .unwrap();
    let first = f.bd.show(&f.decisions[0]).await.unwrap();
    assert!(held(&f.bd, &f.journal(), &f.source).await.unwrap());
    assert_eq!(first, f.bd.show(&f.decisions[0]).await.unwrap());
    let repaired = f.bd.show(&f.decisions[1]).await.unwrap();
    assert_eq!(repaired.status, Status::Blocked);
    assert!(
        repaired
            .notes
            .as_ref()
            .unwrap()
            .contains("human history to retain")
    );
    assert!(held(&f.bd, &f.journal(), &f.source).await.unwrap());
    assert_eq!(repaired, f.bd.show(&f.decisions[1]).await.unwrap());
}

#[cfg(test)]
#[tokio::test]
async fn restart_acknowledges_completed_release_without_reopening_work() {
    let f = Fixture::new().await;
    assert!(matches!(f.admit().await, AgentOutcome::Waiting { .. }));
    for id in &f.decisions {
        f.bd.close(id, Some("human answer")).await.unwrap();
    }
    let mut entry = f.journal().load(&f.source.id).unwrap().unwrap();
    entry.phase = Phase::ReleasePending;
    f.journal().save(&entry).unwrap();
    f.bd.update(
        &f.source.id,
        UpdateOpts {
            if_status: Some(Status::Open),
            remove_labels: vec![HOLD_LABEL.into()],
            unset_metadata: vec![HOLD_KEY.into()],
            notes: Some("retained source draft".into()),
            set_metadata: vec![("operator.note".into(), "keep".into())],
            ..UpdateOpts::default()
        },
    )
    .await
    .unwrap();
    let before = f.bd.show(&f.source.id).await.unwrap();
    assert!(!held(&f.bd, &f.journal(), &f.source).await.unwrap());
    assert_eq!(before, f.bd.show(&f.source.id).await.unwrap());
    assert_eq!(
        f.journal().load(&f.source.id).unwrap().unwrap().phase,
        Phase::Released
    );
}

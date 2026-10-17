use super::*;

#[cfg(test)]
#[tokio::test]
async fn legacy_migration_requires_review_of_independent_human_state() {
    let f = Fixture::new().await;
    f.journal()
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
    f.bd.update(
        &f.source.id,
        UpdateOpts {
            status: Some(Status::Deferred),
            notes: Some("independent human hold".into()),
            ..UpdateOpts::default()
        },
    )
    .await
    .unwrap();
    let before = f.bd.show(&f.source.id).await.unwrap();
    let error = held(&f.bd, &f.journal(), &before).await.unwrap_err();
    assert!(error.to_string().contains("migration requires review"));
    assert_eq!(f.bd.show(&f.source.id).await.unwrap(), before);
    for id in &f.decisions {
        assert_eq!(
            f.bd.show(id).await.unwrap().parent.as_ref(),
            Some(&f.source.id)
        );
    }
    assert!(f.journal().has_pending_for(&f.root).unwrap());
}

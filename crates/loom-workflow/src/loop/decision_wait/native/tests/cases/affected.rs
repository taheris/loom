#[cfg(test)]
mod cases {
    use super::super::*;

    #[tokio::test]
    async fn admitted_decision_parks_only_eligible_affected_work() {
        let f = Fixture::new().await;
        let producer =
            f.bd.create(CreateOpts {
                title: "independent discovering work".into(),
                parent: Some(f.root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let running =
            f.bd.create(CreateOpts {
                title: "another running dispatch".into(),
                parent: Some(f.root.clone()),
                status: Some(Status::InProgress),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let human =
            f.bd.create(CreateOpts {
                title: "human-owned hold".into(),
                parent: Some(f.root.clone()),
                status: Some(Status::Blocked),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let decision = f.bd.create(CreateOpts {
        title: "discovered decision".into(), issue_type: Some(IssueType::Decision), parent: Some(producer.clone()),
        description: "## Options — choose\n\n### Option 1 — Keep\nCost: debt.\n\n### Option 2 — Change\nCost: churn.\n".into(),
        ..CreateOpts::default()
    }).await.unwrap();
        for id in [&f.source.id, &running, &human] {
            f.bd.dep_add(id, &decision).await.unwrap();
        }
        f.bd.update(&decision, UpdateOpts {
        set_metadata: vec![("loom.decision_candidate".into(), serde_json::json!({
            "version": 1, "producer": producer, "root": f.root, "affected_work": [f.source.id, running, human]
        }).to_string())], ..UpdateOpts::default()
    }).await.unwrap();
        let before_running = f.bd.show(&running).await.unwrap();
        let before_human = f.bd.show(&human).await.unwrap();
        let producer = f.bd.show(&producer).await.unwrap();
        let wire = format!(
            "LOOM_CLARIFY: {{\"decisions\":[\"{decision}\"]}}\nLOOM_RETRY: {{\"reason\":\"independent repair remains\"}}"
        );
        let outcome = crate::r#loop::reconcile_session(
            &f.bd,
            &f.journal(),
            &producer,
            crate::r#loop::SessionResult::Decoded {
                outcome: loom_driver::agent::SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                },
                output: Arc::new(loom_protocol::output::decode(
                    &wire,
                    loom_protocol::output::Phase::Loop,
                )),
            },
            None,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, AgentOutcome::Retry { .. }), "{outcome:?}");
        assert_eq!(f.bd.show(&producer.id).await.unwrap(), producer);
        assert_eq!(
            f.bd.show(&f.source.id).await.unwrap().status,
            Status::Blocked
        );
        assert_eq!(f.bd.show(&running).await.unwrap(), before_running);
        assert_eq!(f.bd.show(&human).await.unwrap(), before_human);
        f.close(&decision).await;
        f.refresh().await;
        assert_eq!(
            f.bd.show(&f.source.id).await.unwrap().status,
            Status::Blocked
        );
        f.close(&f.blockers[0]).await;
        f.close(&f.blockers[1]).await;
        f.refresh().await;
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
    }

    #[tokio::test]
    async fn explicit_native_wait_admits_its_own_in_progress_source() {
        let mut f = Fixture::new().await;
        f.bd.update(
            &f.source.id,
            UpdateOpts {
                status: Some(Status::InProgress),
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        f.source = f.bd.show(&f.source.id).await.unwrap();
        f.admit().await;
        assert_eq!(
            f.bd.show(&f.source.id).await.unwrap().status,
            Status::Blocked
        );
        f.close(&f.blockers[0]).await;
        f.close(&f.blockers[1]).await;
        f.refresh().await;
        let source = f.bd.show(&f.source.id).await.unwrap();
        assert_eq!(source.status, Status::Open);
        assert_eq!(source.notes, f.source.notes);
        assert_eq!(source.metadata, f.source.metadata);
    }
}
